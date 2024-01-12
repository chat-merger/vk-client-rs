#![allow(dead_code, clippy::future_not_send)]

// TODO: add docs
//       infallible deserialization
//       complete project structure
//       ??

mod api;
mod ext;
mod grpc {
    #![allow(
        clippy::similar_names,
        unused_mut,
        clippy::doc_markdown,
        clippy::missing_const_for_fn,
        clippy::trivially_copy_pass_by_ref,
        clippy::use_self,
        clippy::wildcard_imports,
        clippy::default_trait_access
    )]

    tonic::include_proto!("mergerapi");
}

use std::{env, fmt, sync::Arc};

use futures::{future::LocalBoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_aux::field_attributes::deserialize_number_from_string;
use serde_repr::Deserialize_repr;
use tokio_stream::wrappers::ReceiverStream;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer};
use url::Url;

use crate::ext::ResponseJsonErrorPath as _;

const GROUP_ID: u64 = 224_192_083;
const AUTH_HEADER: &str = "x-api-key";
const PEER_ID: u64 = 2_000_000_002;

fn init() -> color_eyre::Result<()> {
    dotenvy::dotenv()?;
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(LevelFilter::DEBUG))
        .with(tracing_error::ErrorLayer::default())
        .init();

    env::set_var("RUST_SPANTRACE", "1");
    env::set_var("RUST_LIB_BACKTRACE", "1");

    Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    init()?;

    let token = env::var("VK_API_TOKEN")?;
    let api_key = env::var("X_API_KEY")?;
    let host: tonic::transport::Uri = env::var("SERVER_HOST")?.parse()?;

    let (tx, rx) = tokio::sync::mpsc::channel(100);

    let mut client = {
        let channel = tonic::transport::Channel::builder(host).connect().await?;
        grpc::base_service_client::BaseServiceClient::new(channel)
    };

    let request = {
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.append(AUTH_HEADER, api_key.parse()?);
        let exts = tonic::Extensions::default();

        tonic::Request::from_parts(meta, exts, ReceiverStream::new(rx))
    };

    let bot = Bot::new(token, GROUP_ID);

    tokio::spawn({
        let bot = bot.clone();
        async move {
            let response = client.connect(request).await?;

            let mut stream = response.into_inner();
            while let Some(response) = stream.message().await? {
                tracing::debug!("{response:#?}");

                let grpc::Response {
                    reply_msg_id,
                    author,
                    client,
                    body,
                    ..
                } = response;

                let text = body.as_ref().map_or_else(
                    || "no text",
                    |b| match b {
                        grpc::response::Body::Text(t) => t.value.as_str(),
                        grpc::response::Body::Media(_) => "",
                    },
                );
                let reply = reply_msg_id
                    .as_deref()
                    .map(|id| format!("reply to: {id}"))
                    .unwrap_or_default();
                let author = author.as_deref().unwrap_or_default();

                bot.send_message(
                    PEER_ID,
                    format!(
                        r#"
{reply}
[{author} from {client}]: {text}"#
                    ),
                )
                .await?;
            }

            color_eyre::Result::<_, color_eyre::Report>::Ok(())
        }
    });

    let on_message = {
        let tx = tx.clone();
        let bot = bot.clone();
        move |message: Message| {
            let tx = tx.clone();
            let bot = bot.clone();
            async move {
                let author = if message.from_id > 0 {
                    let [UsersGetResponse {
                        first_name,
                        last_name,
                        ..
                    }]: [UsersGetResponse; 1] = get(
                        &bot.client,
                        "users.get",
                        &[
                            ("user_ids", message.from_id.to_string().as_str()),
                            ("access_token", bot.token.as_str()),
                            ("v", "5.199"),
                        ],
                    )
                    .await?;
                    format!("{first_name} {last_name}")
                } else {
                    "Unknown".to_owned()
                };

                tx.send(grpc::Request {
                    reply_msg_id: message.reply.map(|r| r.id.to_string()),
                    created_at: message.date.try_into()?,
                    author: Some(author),
                    is_silent: false,
                    body: Some(grpc::request::Body::Text(grpc::Text {
                        format: grpc::text::Format::Plain as i32,
                        value: message.text,
                    })),
                })
                .await?;

                Ok(())
            }
            .boxed_local()
        }
    };

    bot.on_message(on_message).run().await?;

    Ok(())
}

type Callback =
    Arc<dyn Fn(Message) -> LocalBoxFuture<'static, color_eyre::Result<()>> + Send + Sync>;

#[derive(Clone)]
struct Bot {
    token: String,
    client: reqwest::Client,
    group_id: u64,
    wait: usize,
    on_message: Option<Callback>,
}

impl fmt::Debug for Bot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bot")
            .field("token", &self.token)
            .field("client", &self.client)
            .field("group_id", &self.group_id)
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

impl Bot {
    fn new(token: impl Into<String>, group_id: u64) -> Self {
        Self {
            token: token.into(),
            client: reqwest::Client::new(),
            group_id,
            wait: 25,
            on_message: None,
        }
    }

    fn on_message<F>(mut self, callback: F) -> Self
    where
        F: Fn(Message) -> LocalBoxFuture<'static, color_eyre::Result<()>> + Send + Sync + 'static,
    {
        self.on_message = Some(Arc::new(callback));
        self
    }

    #[tracing::instrument(skip_all)]
    async fn run(&self) -> color_eyre::Result<()> {
        let GetLongPollServerResponse {
            key,
            server,
            mut ts,
        } = get(
            &self.client,
            "groups.getLongPollServer",
            &[
                ("group_id", self.group_id.to_string().as_str()),
                ("access_token", self.token.as_str()),
                ("v", "5.199"),
            ],
        )
        .await?;

        loop {
            let resp = self
                .client
                .get(format!(
                    "{server}?act=a_check&key={key}&ts={ts}&wait={wait}",
                    wait = self.wait,
                ))
                .send()
                .await?
                .json_error_path::<serde_json::Value>()
                .await?;

            let new_ts: i32 = resp
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .map_or_else(|| panic!("error getting `ts` field!"), |ts| ts);

            ts = new_ts;

            let LongPollingResponse {
                ts: new_ts,
                updates,
            } = match serde_json::from_value(resp.clone()) {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::debug!("{resp:#?}");
                    tracing::error!("error deserializing `LongPollingResponse: {e}");
                    continue;
                }
            };

            for update in updates {
                tracing::debug!(?update, "update");
                match update {
                    Update::MessageNew {
                        object: Object { message, .. },
                        ..
                    } => {
                        if let Some(cb) = &self.on_message {
                            cb(message).await.map_err(|err| {
                                tracing::error!("{err}");
                                err
                            })?;
                        }
                    }
                }
            }

            ts = new_ts;
        }
    }

    #[tracing::instrument(skip_all)]
    async fn send_message(&self, peer_id: u64, text: impl AsRef<str>) -> color_eyre::Result<()> {
        let text = text.as_ref();
        let sent_id: u64 = get(
            &self.client,
            "messages.send",
            &[
                ("peer_id", peer_id.to_string().as_str()),
                ("random_id", "0"),
                ("message", text),
                ("group_id", self.group_id.to_string().as_str()),
                ("access_token", self.token.as_str()),
                ("v", "5.199"),
            ],
        )
        .await?;

        tracing::debug!(sent_id, "message sent");

        Ok(())
    }
}

#[tracing::instrument(skip(client, query))]
async fn get<Response>(
    client: &reqwest::Client,
    method: &'static str,
    query: &(impl Serialize + Sized),
) -> color_eyre::Result<Response>
where
    Response: for<'de> Deserialize<'de>,
{
    let res: Result<_, _> = client
        .get(format!("http://api.vk.com/method/{method}"))
        .query(query)
        .send()
        .await?
        .json_error_path::<VkResponse<Response>>()
        .await?
        .into();
    Ok(res?)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
enum VkResponse<Response> {
    #[serde(rename = "response")]
    Response(Response),
    #[serde(rename = "error")]
    Error(Error),
}

impl<Response> From<VkResponse<Response>> for Result<Response, Error> {
    fn from(value: VkResponse<Response>) -> Self {
        match value {
            VkResponse::Response(res) => Ok(res),
            VkResponse::Error(err) => Err(err),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Error {
    #[serde(rename = "error_code")]
    code: ErrorCode,
    #[serde(rename = "error_msg")]
    message: String,
    request_params: Vec<RequestParam>,
}

#[derive(Debug, Deserialize_repr, Clone, Copy)]
#[serde(deny_unknown_fields)]
#[repr(usize)]
enum ErrorCode {
    NotFound = 104,
    CanTSendMessagesForUsersFromBlacklist = 900,
    CanTSendMessagesForUsersWithoutPermission = 901,
    CanTSendMessagesToThisUserDueToTheirPrivacySettings = 902,
    KeyboardFormatIsInvalid = 911,
    ThisIsAChatBotFeatureChangeThisStatusInSettings = 912,
    TooManyForwardedMessages = 913,
    MessageIsTooLong = 914,
    YouDonTHaveAccessToThisChat = 917,
    CanTForwardTheseMessages = 921,
    YouLeftThisChat = 922,
    YouAreNotAdminOfThisChat = 925,
    ContactNotFound = 936,
    TooManyPostsInMessages = 940,
    CannotUseThisIntent = 943,
    LimitsOverflowForThisIntent = 944,
    ChatWasDisabled = 945,
    ChatNotSupported = 946,
    CanTSendMessageReplyTimedOut = 950,
    YouCanTAccessDonutChatWithoutSubscription = 962,
    MessageCannotBeForwarded = 969,
    AppActionIsRestrictedForConversationsWithCommunities = 979,
    YouAreRestrictedToWriteToAChat = 983,
    YouHasSpamRestriction = 984,
    WritingIsDisabledForThisChat = 1012,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            code,
            message,
            request_params,
        } = self;
        writeln!(f, "{message} ({code:?}#{}):", *code as usize)?;
        for param in request_params {
            writeln!(
                f,
                "   {key}: {value},",
                key = param.key,
                value = param.value,
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestParam {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetLongPollServerResponse {
    key: String,
    server: Url,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    ts: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsersGetResponse {
    id: u64,
    first_name: String,
    last_name: String,
    can_access_closed: bool,
    is_closed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LongPollingResponse {
    #[serde(deserialize_with = "deserialize_number_from_string")]
    ts: i32,
    #[serde(rename = "updates")]
    updates: Vec<Update>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
enum Update {
    #[serde(rename = "message_new")]
    MessageNew {
        group_id: i32,
        event_id: String,
        #[serde(deserialize_with = "deserialize_number_from_string")]
        v: f64,
        object: Object,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
    message: Message,
    client_info: ClientInfo,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientInfo {
    button_actions: Vec<String>,
    keyboard: bool,
    inline_keyboard: bool,
    carousel: bool,
    lang_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    date: u64,
    from_id: i64,
    id: u64,
    out: u64,
    version: u64,
    attachments: Vec<Attachment>,
    conversation_message_id: u64,
    fwd_messages: Vec<ReplyMessage>,
    important: bool,
    is_hidden: bool,
    peer_id: u64,
    random_id: u64,
    text: String,
    is_unavailable: bool,
    #[serde(rename = "reply_message")]
    reply: Option<ReplyMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyMessage {
    attachments: Vec<Attachment>,
    conversation_message_id: u64,
    date: u64,
    from_id: i64,
    id: u64,
    peer_id: u64,
    text: String,
    #[serde(default)]
    fwd_messages: Vec<ReplyMessage>,
    #[serde(rename = "reply_message")]
    reply: Option<Box<ReplyMessage>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    r#type: String,
    #[serde(flatten)]
    att: AttachmentType,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
enum AttachmentType {
    #[serde(rename = "photo")]
    Photo {
        album_id: i64,
        date: u64,
        id: u64,
        owner_id: u64,
        access_key: String,
        sizes: Vec<Size>,
        text: String,
        web_view_token: String,
        has_tags: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    height: u64,
    r#type: SizeType,
    width: u64,
    url: Url,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
enum SizeType {
    #[serde(rename = "s")]
    Small,
    #[serde(rename = "m")]
    Medium,
    #[serde(rename = "x")]
    X,
    #[serde(rename = "y")]
    Y,
    #[serde(rename = "z")]
    Z,
    #[serde(rename = "w")]
    W,
    #[serde(rename = "o")]
    O,
    #[serde(rename = "p")]
    P,
    #[serde(rename = "q")]
    Q,
    #[serde(rename = "r")]
    R,
}
