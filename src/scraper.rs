mod deviantart;
mod nitter;
mod philomena;
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
    let is_philomena = url_check_cache.wrap(
        (&url, "philomena"),
        Duration::seconds(config.cache_check_duration as i64),
        philomena::is_philomena(&url),
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
    } else if is_philomena.await.unwrap_or(false) {
        Ok(philomena::philomena_scrape(config, &url, db)
            .await
            .context("Philomena parser failed")?)
    } else {
        Ok(None)
    }
}
