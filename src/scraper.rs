mod deviantart;
mod nitter;
mod raw;
mod tumblr;
mod twitter;

use anyhow::{Context, Result};
use futures_cache::{Cache, Duration};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use visit_diff::Diff;

use crate::Configuration;

#[cfg(not(test))]
pub type UrlT = url::Url;

#[cfg(not(test))]
#[inline]
pub fn from_url(f: url::Url) -> UrlT {
    f
}

#[cfg(not(test))]
pub fn url_to_str(f: &UrlT) -> String {
    f.to_string()
}

#[cfg(test)]
pub type UrlT = String;

#[cfg(test)]
#[inline]
pub fn from_url(f: url::Url) -> UrlT {
    f.to_string()
}

#[cfg(test)]
pub fn url_to_str(f: &UrlT) -> String {
    f.to_string()
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Diff))]
#[serde(untagged)]
pub enum ScrapeResult {
    Err(ScrapeResultError),
    Ok(ScrapeResultData),
    None,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Diff))]
pub struct ScrapeResultError {
    errors: Vec<String>,
}

impl From<String> for ScrapeResultError {
    fn from(f: String) -> Self {
        Self { errors: vec![f] }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Diff))]
pub struct ScrapeResultData {
    source_url: Option<UrlT>,
    author_name: Option<String>,
    description: Option<String>,
    images: Vec<ScrapeImage>,
}

impl Default for ScrapeResult {
    fn default() -> Self {
        Self::None
    }
}

impl ScrapeResult {
    pub fn from_err(e: anyhow::Error) -> ScrapeResult {
        ScrapeResult::Err(ScrapeResultError {
            errors: {
                let mut errors = Vec::new();
                for e in e.chain() {
                    if !e.is::<reqwest::Error>() {
                        errors.push(e)
                    }
                }
                errors.iter().map(|e| format!("{}", e)).collect()
            },
        })
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq)]
#[cfg_attr(test, derive(Diff))]
pub struct ScrapeImage {
    url: UrlT,
    camo_url: UrlT,
}

impl PartialEq for ScrapeImage {
    fn eq(&self, other: &Self) -> bool {
        url_to_str(&self.url).eq_ignore_ascii_case(&url_to_str(&other.url))
            && url_to_str(&self.camo_url).eq_ignore_ascii_case(&url_to_str(&other.camo_url))
    }
}

pub fn client(config: &Configuration) -> Result<reqwest::Client> {
    client_with_redir_limit(config, reqwest::redirect::Policy::none())
}

pub fn client_with_redir_limit(
    config: &Configuration,
    redir_policy: reqwest::redirect::Policy,
) -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(5000))
        .connect_timeout(std::time::Duration::from_millis(2500))
        .user_agent("Mozilla/5.0")
        .cookie_store(true)
        .redirect(redir_policy);
    let client = match config.proxy_url.clone() {
        None => client,
        Some(proxy_url) => {
            use reqwest::Proxy;
            use std::str::FromStr;
            let proxy_url = url::Url::from_str(&proxy_url)?;
            let proxy = match proxy_url.scheme() {
                "http" => Proxy::all(proxy_url)?,
                "https" => Proxy::all(proxy_url)?,
                "socks" => Proxy::all(proxy_url)?,
                "socks5" => Proxy::all(proxy_url)?,
                _ => anyhow::bail!(
                    "unknown client proxy protocol, specify http, https, socks or socks5"
                ),
            };
            client.proxy(proxy)
        }
    };
    Ok(client.build()?)
}

pub async fn scrape(
    config: &Configuration,
    db: &sled::Db,
    url: &str,
) -> Result<Option<ScrapeResult>> {
    use std::str::FromStr;
    let url = url::Url::from_str(url).context("could not parse URL for scraper")?;
    let url_check_cache = Cache::load(db.open_tree("check_cache")?)?;
    let is_twitter = url_check_cache.wrap(
        (&url, "twitter"),
        Duration::seconds(config.cache_check_duration as i64),
        twitter::is_twitter(&url),
    );
    let is_nitter = url_check_cache.wrap(
        (&url, "nitter"),
        Duration::seconds(config.cache_check_duration as i64),
        nitter::is_nitter(&url),
    );
    let is_tumblr = url_check_cache.wrap(
        (&url, "tumblr"),
        Duration::seconds(config.cache_check_duration as i64),
        tumblr::is_tumblr(&url),
    );
    let is_deviantart = url_check_cache.wrap(
        (&url, "deviantart"),
        Duration::seconds(config.cache_check_duration as i64),
        deviantart::is_deviantart(&url),
    );
    let is_raw = url_check_cache.wrap(
        (&url, "raw"),
        Duration::seconds(config.cache_check_duration as i64),
        raw::is_raw(&url, config),
    );
    if is_twitter.await.unwrap_or(false) {
        Ok(twitter::twitter_scrape(config, &url, db)
            .await
            .context("Twitter parser failed")?)
    } else if is_deviantart.await.unwrap_or(false) {
        Ok(deviantart::deviantart_scrape(config, &url, db)
            .await
            .context("DeviantArt parser failed")?)
    } else if is_tumblr.await.unwrap_or(false) {
        Ok(tumblr::tumblr_scrape(config, &url, db)
            .await
            .context("Tumblr parser failed")?)
    } else if is_raw.await.unwrap_or(false) {
        Ok(raw::raw_scrape(config, &url, db)
            .await
            .context("Raw parser failed")?)
    } else if is_nitter.await.unwrap_or(false) {
        Ok(nitter::nitter_scrape(config, &url, db)
            .await
            .context("Nitter parser failed")?)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod test {
    use log::warn;

    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_twitter_scraper() -> Result<()> {
        crate::LOGGER.flush();
        let tweet = r#"https://twitter.com/TheOnion/status/1372594920427491335?s=20"#;
        let config = Configuration::default();
        let db = sled::Config::default().temporary(true).open()?;
        let mut parsed = url::Url::from_str(tweet)?;
        parsed.set_fragment(None);
        parsed.set_query(None);
        let scrape = tokio_test::block_on(scrape(&config, &db, tweet));
        let scrape = match scrape {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let mut scrape = match scrape {
            Some(s) => s,
            None => anyhow::bail!("got none response from scraper"),
        };
        let test_results_expected = ScrapeImage {
            url: from_url(url::Url::from_str(
                "https://pbs.twimg.com/media/EwxvzkEXAAMFg7k.jpg",
            )?),
            camo_url: from_url(url::Url::from_str(
                "https://pbs.twimg.com/media/EwxvzkEXAAMFg7k.jpg",
            )?),
        };
        match &mut scrape {
            ScrapeResult::Ok(scrape) => {
                for test_result in scrape.images.iter() {
                    assert_eq!(&test_results_expected, test_result);
                }
                scrape.images = Vec::new();
            }
            ScrapeResult::Err(e) => assert!(false, "error in scrape: {:?}", e.errors),
            ScrapeResult::None => assert!(false, "no data in scrape"),
        }
        visit_diff::assert_eq_diff!(ScrapeResult::Ok(ScrapeResultData{
            source_url: Some(from_url(parsed)),
            author_name: Some("TheOnion".to_string()),
            description: Some("Deal Alert: The Federal Government Is Cutting You A $1,400 Stimulus Check That You Can, And Should, Spend Exclusively On 93 Copies Of ‘Stardew Valley’ https://t.co/RuRZN4XWIK https://t.co/tclZn8dQgg".to_string()),
            images: Vec::new(),
        }), scrape);
        Ok(())
    }

    #[test]
    fn test_nitter_scraper() -> Result<()> {
        crate::LOGGER.flush();
        let host = &crate::scraper::nitter::NITTER_INSTANCES;
        let host = { &host[random_number::random!(..(host.len()))] };
        let tweet = format!(
            r#"https://{}/TheOnion/status/1372594920427491335?s=20"#,
            host
        );
        let config = Configuration::default();
        let db = sled::Config::default().temporary(true).open()?;

        let scrape = tokio_test::block_on(scrape(&config, &db, &tweet))?.unwrap();
        visit_diff::assert_eq_diff!(ScrapeResult::Ok(ScrapeResultData{
            source_url: Some(from_url(url::Url::from_str(r#"https://twitter.com/TheOnion/status/1372594920427491335?s=20"#)?)),
            author_name: Some("TheOnion".to_string()),
            description: Some("Deal Alert: The Federal Government Is Cutting You A $1,400 Stimulus Check That You Can, And Should, Spend Exclusively On 93 Copies Of ‘Stardew Valley’ bit.ly/3bX25sQ".to_string()),
            images: vec![
                ScrapeImage {
                    url: from_url(url::Url::from_str(
                        &format!("https://{}/pic/media%2FEwxvzkEXAAMFg7K.jpg%3Fname%3Dorig?s=20", host),
                    )?),
                    camo_url: from_url(url::Url::from_str(
                        &format!("https://{}/pic/media%2FEwxvzkEXAAMFg7K.jpg%3Fname%3Dorig?s=20", host),
                    )?),
                }
            ]
        }), scrape);
        Ok(())
    }

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

    #[test]
    fn test_tumblr_scraper() -> Result<()> {
        crate::LOGGER.flush();
        let url = r#"https://tcn1205.tumblr.com/post/186904081532/in-wonderland"#;
        let config = Configuration::default();
        if config.tumblr_api_key.is_none() {
            warn!("Tumblr API key not configured, skipping");
            return Ok(());
        }
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
        let expected_result = ScrapeResult::Ok(ScrapeResultData{
            source_url: Some("https://tcn1205.tumblr.com/post/186904081532/in-wonderland".to_string()),
            author_name: Some("tcn1205".to_string()),
            description: Some("In Wonderland.".to_string()),
            images: vec![
                ScrapeImage{
                    url: "https://64.media.tumblr.com/cf3b6e5981e0aaf0f1be305429faa6c4/tumblr_pw0dzrDNvN1vlyxx7o1_1280.png".to_string(),
                    camo_url: "https://64.media.tumblr.com/cf3b6e5981e0aaf0f1be305429faa6c4/tumblr_pw0dzrDNvN1vlyxx7o1_400.png".to_string(),
                }
            ],
        });
        visit_diff::assert_eq_diff!(expected_result, scrape);
        Ok(())
    }

    #[test]
    fn test_deviantart_scraper() -> Result<()> {
        crate::LOGGER.flush();
        let url = r#"https://www.deviantart.com/the-park/art/Comm-Baseball-cap-derpy-833396912"#;
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
        let expected_result = ScrapeResult::Ok(ScrapeResultData{
            source_url: Some("https://www.deviantart.com/the-park/art/Comm-Baseball-cap-derpy-833396912".to_string()),
            author_name: Some("the-park".to_string()),
            description: None,
            images: vec![
                ScrapeImage{
                    url: "https://images-wixmp-ed30a86b8c4ca887773594c2.wixmp.com/f/39da62f1-b049-4f7a-b10b-4cc5167cb9a2/dds6l68-3084d503-abbf-4f6d-bd82-7a36298e0106.png?token=eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1cm46YXBwOiIsImlzcyI6InVybjphcHA6Iiwib2JqIjpbW3sicGF0aCI6IlwvZlwvMzlkYTYyZjEtYjA0OS00ZjdhLWIxMGItNGNjNTE2N2NiOWEyXC9kZHM2bDY4LTMwODRkNTAzLWFiYmYtNGY2ZC1iZDgyLTdhMzYyOThlMDEwNi5wbmcifV1dLCJhdWQiOlsidXJuOnNlcnZpY2U6ZmlsZS5kb3dubG9hZCJdfQ.j20TCPeDfcyM_ATsITz4e7L4Kj2xFa2ZDQ8ul6694dE".to_string(),
                    camo_url: "https://images-wixmp-ed30a86b8c4ca887773594c2.wixmp.com/f/39da62f1-b049-4f7a-b10b-4cc5167cb9a2/dds6l68-3084d503-abbf-4f6d-bd82-7a36298e0106.png?token=eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1cm46YXBwOiIsImlzcyI6InVybjphcHA6Iiwib2JqIjpbW3sicGF0aCI6IlwvZlwvMzlkYTYyZjEtYjA0OS00ZjdhLWIxMGItNGNjNTE2N2NiOWEyXC9kZHM2bDY4LTMwODRkNTAzLWFiYmYtNGY2ZC1iZDgyLTdhMzYyOThlMDEwNi5wbmcifV1dLCJhdWQiOlsidXJuOnNlcnZpY2U6ZmlsZS5kb3dubG9hZCJdfQ.j20TCPeDfcyM_ATsITz4e7L4Kj2xFa2ZDQ8ul6694dE".to_string(),
                }
            ],
        });
        visit_diff::assert_eq_diff!(expected_result, scrape);
        Ok(())
    }
}
