use crate::scraper::{ScrapeResult, ScrapeResultData};
use crate::{scraper::ScrapeImage, Configuration};
use anyhow::Result;
use ref_thread_local::{ref_thread_local, RefThreadLocal};
use url::Url;

ref_thread_local! {
    static managed MIME_TYPES: Vec<String> = Vec::from([
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
        Ok(MIME_TYPES.borrow().contains(&content_type.to_string()))
    } else {
        Ok(false)
    }
}

pub async fn raw_scrape(
    config: &Configuration,
    url: &Url,
    _db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    Ok(Some(ScrapeResult::Ok(ScrapeResultData {
        source_url: Some(super::from_url(url.clone())),
        author_name: None,
        description: None,
        images: Vec::from([ScrapeImage {
            url: super::from_url(url.clone()),
            camo_url: super::from_url(crate::camo::camo_url(config, url)?),
        }]),
    })))
}

#[cfg(test)]
mod test {
    use crate::scraper::{from_url, scrape};

    use super::*;
    use std::str::FromStr;
    #[test]
    fn test_raw_scraper() -> Result<()> {
        crate::LOGGER.flush();
        let url = r#"https://static.manebooru.art/img/view/2021/3/20/4010154.png"#;
        let config = Configuration::default();
        let db = sled::Config::default().temporary(true).open()?;
        let scrape = tokio_test::block_on(scrape(&config, &db, url));
        let scrape = match scrape {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let scrape = match scrape {
            Some(s) => s,
            None => anyhow::bail!("got none response from scraper"),
        };
        let expected_result = ScrapeResult::Ok(ScrapeResultData {
            source_url: Some(from_url(url::Url::from_str(url)?)),
            author_name: None,
            description: None,
            images: Vec::from([ScrapeImage {
                url: from_url(url::Url::from_str(url)?),
                camo_url: from_url(url::Url::from_str(url)?),
            }]),
        });
        visit_diff::assert_eq_diff!(expected_result, scrape);
        Ok(())
    }
}
