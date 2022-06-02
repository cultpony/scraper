use std::{ops::Index, str::FromStr};

use crate::scraper::ScrapeResult;
use crate::scraper::ScrapeResultData;
use crate::{scraper::ScrapeImage, Configuration};
use anyhow::{Context, Result};
use futures_cache::{Cache, Duration};
use log::trace;
use regex::Regex;
use serde_json::Value;
use url::Url;

const ACTIVATION_URL: &str = "https://api.twitter.com/1.1/guest/activate.json";

lazy_static::lazy_static! {
    static ref URL_REGEX: Regex = Regex::from_str(r#"\Ahttps?://(?:mobile\.)?twitter.com/([A-Za-z\d_]+)/status/([\d]+)/?"#)
        .expect("failure in setting up essential regex");
    static ref SCRIPT_REGEX: Regex = Regex::from_str(r#"="(https://abs.twimg.com/responsive-web/client-web(?:-legacy)?/main\.[\da-z]+\.js)"#)
        .expect("failure in setting up essential regex");
    static ref BEARER_REGEX: Regex = Regex::from_str(r#"(AAAAAAAAAAAAA[^"]*)"#)
        .expect("failure in setting up essential regex");
}

pub async fn is_twitter(url: &Url) -> Result<bool> {
    if URL_REGEX.is_match_at(url.as_str(), 0) {
        return Ok(true);
    }
    Ok(false)
}

async fn twitter_page_request(client: &reqwest::Client, page_url: &str) -> Result<String> {
    trace!("making page request: {}", page_url);
    client
        .get(page_url)
        .send()
        .await
        .with_context(|| format!("could not get api_data request @ {page_url}"))?
        .error_for_status()
        .with_context(|| format!("bad status code for api_data request @ {page_url}"))?
        .text()
        .await
        .with_context(|| format!("could not read api data response from {page_url}"))
}

async fn get_script_data(client: &reqwest::Client, url: &str) -> Result<String> {
    trace!("making script request: {}", url);
    client
        .get(url)
        .send()
        .await
        .context("could not get script_data request")?
        .error_for_status()
        .context("bad status for script data request")?
        .text()
        .await
        .context("could not read script_data response")
}

async fn get_gt_token(client: &reqwest::Client, bearer: &str) -> Result<String> {
    trace!("making GT activation request");
    let v = client
        .post(ACTIVATION_URL.to_string())
        .header("Authorization", format!("Bearer {}", bearer))
        .send()
        .await
        .context("could not complete activation request")?
        .error_for_status()
        .context("bad status for GT activation")?
        .json::<serde_json::Value>()
        .await
        .context("could not read GT response")?;
    let guest_token = v.as_object().unwrap().get("guest_token");
    match guest_token {
        Some(guest_token) => Ok(match guest_token.as_str() {
            Some(v) => v.to_string(),
            None => anyhow::bail!("invalid GT in twitter API response"),
        }),
        None => anyhow::bail!("no GT in twitter API response"),
    }
}

async fn make_api_request(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
    gt: &str,
) -> Result<Value> {
    trace!("making api request: {url}");
    let req = client
        .get(url)
        .header("Authorization", format!("Bearer {}", bearer))
        .header("x-guest-token", gt)
        .build()
        .with_context(|| format!("failed to build client api_request against {url}"))?;
    client
        .execute(req)
        .await
        .context("API request failed")?
        .error_for_status()
        .context("API request is not 200 code")?
        .json()
        .await
        .context("response is not valid json")
}

pub async fn twitter_scrape(
    config: &Configuration,
    url: &Url,
    db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    let reqwest_cache = Cache::load(
        db.open_tree("twitter_request_cache")
            .context("twitter response cache unavailable")?,
    )
    .context("could not load twitter response cache")?;
    let client = crate::scraper::client(config).context("could not create twitter agent")?;
    let (user, status_id) = {
        let caps = URL_REGEX.captures(url.as_str());
        let caps = match caps {
            Some(caps) => caps,
            None => anyhow::bail!("could not parse tweet url"),
        };
        (&caps[1].to_string(), &caps[2].to_string())
    };
    let page_url = format!("https://twitter.com/{}/status/{}", user, status_id);
    let api_url = format!(
        "https://api.twitter.com/2/timeline/conversation/{}.json?tweet_mode=extended",
        status_id
    );
    let url = format!("https://twitter.com/{}/status/{}", user, status_id);

    let (gt, bearer) = {
        let page_url = page_url.clone();
        let api_data = twitter_page_request(&client, &page_url)
            .await
            .context("initial page request failed")?;
        let script_caps: Option<regex::Captures> = SCRIPT_REGEX.captures(&api_data);
        let script_caps = match script_caps {
            Some(v) => v[1].to_string(),
            None => anyhow::bail!("could not get script"),
        };
        log::debug!("script_caps: {:?}", script_caps);
        let script_data = reqwest_cache
            .wrap(
                &script_caps,
                Duration::seconds(config.cache_http_duration as i64),
                get_script_data(&client, &script_caps),
            )
            .await
            .context("invalid script_data response")?;
        let bearer_caps = BEARER_REGEX.captures(&script_data);
        let bearer = match bearer_caps {
            Some(v) => v[0].to_string(),
            None => anyhow::bail!("could not get bearer"),
        };
        let gt = get_gt_token(&client, &bearer)
            .await
            .context("could not get guest token")?;
        (gt, bearer)
    };

    let mut api_response = reqwest_cache
        .wrap(
            (&api_url, &gt, &bearer),
            Duration::seconds(config.cache_http_duration as i64),
            make_api_request(&client, &api_url, &bearer, &gt),
        )
        .await
        .context("invalid api response")?;
    use std::ops::IndexMut;
    let tweet = api_response.index_mut("globalObjects");
    let tweet = tweet.index_mut("tweets");
    let tweet = tweet.index_mut(status_id);
    let page_url = url::Url::from_str(&page_url).context("page url is not valid from API")?;
    let images = {
        let tweet = tweet.clone();
        let media = tweet.index("entities").index("media").as_array();
        let media: Vec<ScrapeImage> = match media {
            None => Vec::new(),
            Some(media) => media
                .iter()
                .flat_map(|x| -> anyhow::Result<ScrapeImage> {
                    let url_orig = x.index("media_url_https").as_str().unwrap_or_default();
                    let url_noorig = url_orig.trim_end_matches(":orig");
                    let url_orig =
                        url::Url::from_str(url_orig).unwrap_or_else(|_| page_url.clone());
                    let url_noorig =
                        url::Url::from_str(url_noorig).unwrap_or_else(|_| page_url.clone());
                    let camo_url: anyhow::Result<Url> = crate::camo::camo_url(config, &url_orig);
                    let camo_url = camo_url.context("could not generate Camo url")?;
                    log::debug!("urls: {}, noorig: {}", url_orig, url_noorig);
                    Ok(ScrapeImage {
                        url: super::from_url(url_noorig),
                        camo_url: super::from_url(camo_url),
                    })
                })
                .collect(),
        };
        media
    };
    if images.is_empty() {
        return Ok(None);
    }
    Ok(Some(ScrapeResult::Ok(ScrapeResultData {
        source_url: Some(super::from_url(
            url::Url::from_str(&url).context("source is not valid URL")?,
        )),
        author_name: Some(user.to_owned()),
        additional_tags: None,
        description: tweet.index("text").as_str().map_or_else(
            || tweet.index("full_text").as_str().map(|f| f.to_owned()),
            |f| Some(f.to_owned()),
        ),
        images,
    })))
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::scraper::{from_url, scrape};
    use std::str::FromStr;

    //TODO: fix twitter test & scraper
    #[test]
    #[ignore = "twitter is too unstable to test properly atm"]
    fn test_twitter_scraper() -> Result<()> {
        crate::LOGGER.lock().unwrap().flush();
        let tweet = r#"https://twitter.com/theprincessxena/status/1532144541523910658"#;
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
            additional_tags: None,
            description: Some("Deal Alert: The Federal Government Is Cutting You A $1,400 Stimulus Check That You Can, And Should, Spend Exclusively On 93 Copies Of ‘Stardew Valley’ https://t.co/RuRZN4XWIK https://t.co/tclZn8dQgg".to_string()),
            images: Vec::new(),
        }), scrape);
        Ok(())
    }
}
