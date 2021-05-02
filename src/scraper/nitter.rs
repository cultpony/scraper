use crate::scraper::{ScrapeResult, ScrapeResultData};
use crate::{scraper::ScrapeImage, Configuration};
use anyhow::Context;
use anyhow::Result;
use lazy_static::lazy_static;
use log::debug;
use ref_thread_local::{ref_thread_local, RefThreadLocal};
use regex::Regex;
use std::str::FromStr;
use url::Url;
use visdom::html::ParseOptions;
use visdom::Vis;

lazy_static! {
    pub static ref NITTER_INSTANCES: Vec<String> = vec![
        "nitter.net",
        "nitter.42l.fr",
        "nitter.nixnet.services",
        "nitter.mastodont.cat",
        "nitter.tedomum.net",
        "nitter.fdn.fr",
        "nitter.kavin.rocks",
        "tweet.lambda.dance",
        "nitter.cc",
        "nitter.vxempire.xyz",
        "nitter.unixfox.eu",
        "nitter.domain.glass",
        "nitter.eu",
        "nitter.ethibox.fr",
        "nitter.namazso.eu",
        "nitter.mailstation.de",
        "nitter.actionsack.com",
        "nitter.cattube.org",
        "nitter.dark.fail",
        "birdsite.xanny.family",
        "nitter.40two.app",
        "nitter.skrep.in",
    ]
    .into_iter()
    .map(std::string::String::from)
    .collect();
}

ref_thread_local! {
    static managed TWEET_REGEX: Regex = Regex::from_str(r#"/([A-Za-z\d_]+)/status/([\d]+)[?#]*.*"#).expect("failure in setting up essential regex");
}

pub async fn is_nitter(url: &Url) -> Result<bool> {
    Ok(match url.host_str() {
        None => false,
        Some(host) => {
            NITTER_INSTANCES.contains(&host.to_string())
                && TWEET_REGEX.borrow().is_match(url.path())
        }
    })
}
pub async fn nitter_scrape(
    config: &Configuration,
    url: &Url,
    _db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    let mut url = url.clone();
    let original_url = url.clone();
    if let Some(preferred_host) = &config.preferred_nitter_instance_host {
        url.set_host(Some(preferred_host))
            .context("could not set preferred host")?;
    }
    let client = crate::scraper::client(config).context("can't get HTTP client")?;
    let dom = client
        .get(url.clone())
        .send()
        .await
        .context("request to nitter failed")?
        .error_for_status()
        .context("nitter request failed")?
        .text()
        .await
        .context("response from nitter was incomplete")?;
    let dom = Vis::load_options_catch(
        &dom,
        ParseOptions {
            allow_self_closing: true,
            auto_fix_unclosed_tag: true,
            auto_fix_unescaped_lt: true,
            auto_fix_unexpected_endtag: true,
            ..Default::default()
        },
        Box::new(|err| {
            debug!("error parsing html document: {}", err);
        }),
    );
    let author = dom.find("div.main-tweet").find(r#"a.username"#);
    let author = author.text();
    let author = author.trim_start_matches('@');
    let description = dom.find(r#"div.tweet-content"#).first();
    let description = description.text();
    let source_url = dom.find(r#"[title="Open in Twitter"]"#).first();
    let source_url = source_url.attr("href");
    let source_url = match source_url {
        None => url,
        Some(url) => url::Url::from_str(&url.to_string())?,
    };
    let images_results: Vec<Result<Option<ScrapeImage>>> = dom
        .find("div.main-tweet")
        .find("div.attachments")
        .find("div.image")
        .map(|index, ele| -> Result<Option<ScrapeImage>> {
            let image_url = Vis::dom(ele).find("a.still-image").first().attr("href");
            let image_url = match image_url.map(|x| x.to_string()) {
                Some(image_url) => image_url,
                None => {
                    debug!("no valid URL attribute in attachment");
                    return Ok(None);
                }
            };
            debug!("found image url {}: '{}'", index, image_url);
            let mut url = original_url.clone();
            url.set_path(&image_url);
            let camo_url = crate::camo::camo_url(config, &url).context("could not camo url")?;
            let camo_url = super::from_url(camo_url);
            let url = super::from_url(url);
            Ok(Some(ScrapeImage { url, camo_url }))
        });
    let mut images = Vec::new();
    for image in images_results {
        match image? {
            Some(v) => images.push(v),
            None => continue,
        }
    }
    Ok(Some(ScrapeResult::Ok(ScrapeResultData {
        source_url: Some(super::from_url(source_url)),
        author_name: Some(author.to_string()),
        description: Some(description.to_string()),
        images,
    })))
}

#[cfg(test)]
mod test {
    use crate::scraper::{from_url, scrape};

    use super::*;
    use std::str::FromStr;

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
}
