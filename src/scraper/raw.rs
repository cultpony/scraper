use crate::scraper::{ScrapeResult, ScrapeResultData};
use crate::{scraper::ScrapeImage, Configuration};
use anyhow::Result;
use url::Url;

lazy_static::lazy_static! {
    static ref MIME_TYPES: Vec<String> = Vec::from([
        "image/gif",
        "image/jpeg",
        "image/png",
        "image/svg",
        "image/svg+xml",
        "video/webm",
    ]).iter().map(|x| x.to_string()).collect();
}

pub async fn is_raw(url: &Url, config: &Configuration) -> Result<bool> {
    let client = crate::scraper::client(config)?;
    let res = client.head(url.clone()).send().await?;
    if res.status() == 200 {
        let content_type = res.headers()["content-type"].to_str()?;
        Ok(MIME_TYPES.contains(&content_type.to_string()))
    } else {
        Ok(false)
    }
}

pub async fn raw_scrape(
    config: &Configuration,
    url: &Url,
    _db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    Ok(Some(ScrapeResult::Ok(ScrapeResultData{
        source_url: Some(super::from_url(url.clone())),
        author_name: None,
        description: None,
        images: Vec::from([ScrapeImage {
            url: super::from_url(url.clone()),
            camo_url: super::from_url(crate::camo::camo_url(config, url)?),
        }]),
    })))
}
