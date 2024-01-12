#![allow(dead_code)]

// TODO: add docs
//       infallible deserialization
//       complete project structure
//       ??

mod api;
mod ext;

use core::fmt;
use std::env;

use serde::{Deserialize, Serialize};
use serde_aux::field_attributes::deserialize_number_from_string;
use serde_repr::Deserialize_repr;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer};
use url::Url;

use crate::ext::ResponseJsonErrorPath as _;

const GROUP_ID: u64 = 224_192_083;

fn init() -> color_eyre::Result<()> {
    dotenvy::dotenv()?;
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(LevelFilter::DEBUG))
        .with(tracing_error::ErrorLayer::default())
        .init();

    env::set_var("RUST_SPANTRACE", "1");
    env::set_var("RUST_LIB_BACKTRACE", "0");

    Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    init()?;

    let token = env::var("VK_API_TOKEN")?;

    Bot::new(token, GROUP_ID).run().await?;

    Ok(())
}

struct Bot {
    token: String,
    client: reqwest::Client,
    group_id: u64,
    wait: usize,
}

impl Bot {
    fn new(token: impl Into<String>, group_id: u64) -> Self {
        Self {
            token: token.into(),
            client: reqwest::Client::new(),
            group_id,
            wait: 25,
        }
    }

    #[tracing::instrument(skip_all)]
    async fn run(self) -> color_eyre::Result<()> {
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

            let new_ts: u32 = resp
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .map_or_else(|| panic!("error getting `ts` field!"), |ts| ts);

            ts = new_ts;

            let LongPollingResponse {
                ts: new_ts,
                updates,
            } = match serde_json::from_value(resp) {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::error!("error deserializing `LongPollingResponse: {e}");
                    continue;
                }
            };

            for update in updates {
                tracing::debug!(?update, "update");
                match update {
                    Update::MessageNew {
                        object:
                            Object {
                                message: m @ Message { peer_id, .. },
                                ..
                            },
                        ..
                    } => {
                        self.send_message(peer_id, format!("{m:#?}")).await?;
                    }
                }
            }

            ts = new_ts;
        }
    }

    #[allow(clippy::future_not_send)]
    #[tracing::instrument(skip_all)]
    async fn send_message(&self, peer_id: u64, text: impl AsRef<str>) -> color_eyre::Result<()> {
        let text = text.as_ref();
        let sent_id: u64 = get(
            &self.client,
            "messages.send",
            &[
                ("peer_id", (peer_id).to_string().as_str()),
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

#[allow(clippy::future_not_send)]
#[tracing::instrument(skip_all)]
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
    ts: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageResponse {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LongPollingResponse {
    #[serde(deserialize_with = "deserialize_number_from_string")]
    ts: u32,
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
    from_id: u64,
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
    from_id: u64,
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
        album_id: u64,
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
