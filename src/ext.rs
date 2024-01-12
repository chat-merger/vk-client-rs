use reqwest::Response;
use serde::de::Deserialize;

pub trait ResponseJsonErrorPath {
    async fn json_error_path<T>(self) -> color_eyre::Result<T>
    where
        T: for<'de> Deserialize<'de>;
}

impl ResponseJsonErrorPath for Response {
    #[tracing::instrument(skip_all)]
    async fn json_error_path<T>(self) -> color_eyre::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let bytes = self.text().await?;
        let de = &mut serde_json::Deserializer::from_str(&bytes);
        let res = serde_path_to_error::deserialize(de).map_err(|e| {
            tracing::error!("{e}: {bytes}");
            e
        });
        Ok(res?)
    }
}
