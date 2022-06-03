mod buzzly;
mod deviantart;
mod nitter;
mod philomena;
mod raw;
mod tumblr;
mod twitter;

use std::sync::Arc;

use anyhow::{Context, Result};
use itertools::Itertools;
use log::debug;
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
    additional_tags: Option<Vec<String>>,
    description: Option<String>,
    images: Vec<ScrapeImage>,
}

impl Default for ScrapeResult {
    fn default() -> Self {
        Self::None
    }
}

impl ScrapeResult {
    pub fn from_err(e: Arc<anyhow::Error>) -> ScrapeResult {
        ScrapeResult::Err(ScrapeResultError {
            errors: {
                let mut errors = Vec::new();
                debug!("request error: {}", e);
                for e in e.chain() {
                    if !e.is::<reqwest::Error>() {
                        debug!("request error chain {}: {}", errors.len(), e);
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
        .user_agent("curl/7.83.1")
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

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Scraper {
    Twitter,
    Nitter,
    Tumblr,
    DeviantArt,
    Philomena,
    Buzzly,
    Raw,
}

impl Scraper {
    async fn get_scraper(config: &Configuration, url: &url::Url) -> Result<Option<Self>> {
        let (r0, r1, r2, r3, r4, r5, r6) = tokio::try_join!(
            async {
                twitter::is_twitter(url)
                    .await
                    .map(|mat| if mat { Some(Self::Twitter) } else { None })
            },
            async {
                nitter::is_nitter(url)
                    .await
                    .map(|mat| if mat { Some(Self::Nitter) } else { None })
            },
            async {
                tumblr::is_tumblr(url)
                    .await
                    .map(|mat| if mat { Some(Self::Tumblr) } else { None })
            },
            async {
                deviantart::is_deviantart(url).await.map(|mat| {
                    if mat {
                        Some(Self::DeviantArt)
                    } else {
                        None
                    }
                })
            },
            async {
                philomena::is_philomena(url).await.map(|mat| {
                    if mat {
                        Some(Self::Philomena)
                    } else {
                        None
                    }
                })
            },
            async {
                buzzly::is_buzzlyart(url)
                    .await
                    .map(|mat| if mat { Some(Self::Buzzly) } else { None })
            },
            async {
                raw::is_raw(url, config)
                    .await
                    .map(|mat| if mat { Some(Self::Raw) } else { None })
            },
        )?;
        let res = vec![r0, r1, r2, r3, r4, r5, r6];
        let res: Vec<Scraper> = res.into_iter().flatten().collect_vec();
        Ok(if res.is_empty() {
            None
        } else if res.len() == 1 {
            Some(res[0])
        } else if res.len() > 1 {
            let mut res = res;
            res.sort();
            Some(res[0])
        } else {
            unreachable!("res must be empty but is {:?}", res);
        })
    }

    async fn execute_scrape(
        self,
        config: &Configuration,
        url: &url::Url,
    ) -> Result<Option<ScrapeResult>> {
        match self {
            Scraper::Twitter => Ok(twitter::twitter_scrape(config, url)
                .await
                .context("Twitter parser failed")?),
            Scraper::Nitter => Ok(nitter::nitter_scrape(config, url)
                .await
                .context("Nitter parser failed")?),
            Scraper::Tumblr => Ok(tumblr::tumblr_scrape(config, url)
                .await
                .context("Tumblr parser failed")?),
            Scraper::DeviantArt => Ok(deviantart::deviantart_scrape(config, url)
                .await
                .context("DeviantArt parser failed")?),
            Scraper::Philomena => Ok(philomena::philomena_scrape(config, url)
                .await
                .context("Philomena parser failed")?),
            Scraper::Buzzly => Ok(buzzly::buzzlyart_scrape(config, url)
                .await
                .context("Buzzly parser failed")?),
            Scraper::Raw => Ok(raw::raw_scrape(config, url)
                .await
                .context("Raw parser failed")?),
        }
    }
}

pub async fn scrape(config: &Configuration, url: &str) -> Result<Option<ScrapeResult>> {
    use std::str::FromStr;
    let url = url::Url::from_str(url).context("could not parse URL for scraper")?;
    match Scraper::get_scraper(config, &url).await? {
        Some(scraper) => scraper.execute_scrape(config, &url).await,
        None => Ok(None),
    }
}
