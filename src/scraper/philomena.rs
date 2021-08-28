use std::str::FromStr;

use futures_cache::{Cache, Duration};
use reqwest::{Client, Url};

use crate::camo::camo_url;
use crate::scraper::philomena::derpibooru::is_derpibooru;
use crate::scraper::{from_url, ScrapeImage, ScrapeResult, ScrapeResultData};
use crate::Configuration;
use anyhow::{Context, Result};
use log::{debug, trace};

mod derpibooru;

pub async fn is_philomena(url: &Url) -> Result<bool> {
    is_derpibooru(url).await
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct PhilomenaApiResponse {
    image: PhilomenaApiImageResponse,
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct PhilomenaApiImageResponse {
    tags: Vec<String>,
    source_url: Option<String>,
    uploader: Option<String>,
    description: Option<String>,
    view_url: String,
}

pub async fn philomena_scrape(
    config: &Configuration,
    url: &Url,
    db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    trace!("converting philo url to api url");
    let api_url = if is_derpibooru(url).await? {
        derpibooru::url_to_api(url)?
    } else {
        anyhow::bail!("Tried URL that isn't known philomena")
    };
    let api_url = match api_url {
        None => anyhow::bail!("URL did not match and returned empty"),
        Some(v) => v.to_string(),
    };
    let reqwest_cache = Cache::load(db.open_tree("philomena_request_cache")?)?;
    let client = crate::scraper::client(config)?;
    let resp: PhilomenaApiResponse = reqwest_cache
        .wrap(
            (&api_url, "api_request"),
            Duration::seconds(config.cache_http_duration as i64),
            make_philomena_api_request(&client, &api_url),
        )
        .await?;
    let image = resp.image;
    let image_view = Url::from_str(&image.view_url)?;
    let description = image.description;
    let description = if description.clone().unwrap_or_default().trim().len() == 0 {
        None
    } else {
        description
    };
    Ok(Some(ScrapeResult::Ok(ScrapeResultData {
        source_url: image
            .source_url
            .map(|x| Url::from_str(&x))
            .transpose()?
            .map(from_url),
        author_name: image
            .tags
            .iter()
            .filter(|x| x.starts_with("artist:"))
            .next()
            .cloned()
            .map(|x| x.strip_prefix("artist:").unwrap().to_string()),
        description,
        images: vec![ScrapeImage {
            camo_url: from_url(camo_url(config, &image_view)?),
            url: from_url(image_view),
        }],
    })))
}

async fn make_philomena_api_request(
    client: &Client,
    api_url: &str,
) -> Result<PhilomenaApiResponse> {
    debug!("running api request");
    Ok(client
        .get(api_url)
        .send()
        .await
        .context("request to philomena failed")?
        .error_for_status()
        .context("philomena returned error code")?
        .json()
        .await
        .context("could not parse philomena")?)
}

#[cfg(test)]
mod test {
    use log::warn;

    use crate::scraper::{scrape, ScrapeResultData};

    use super::*;

    #[test]
    fn test_derpibooru_scraper() -> Result<()> {
        crate::LOGGER.lock().unwrap().flush();
        let urls = vec![
            (
                r#"https://derpibooru.org/images/1426211"#,
                ScrapeResultData {
                    source_url: Some("http://brunomilan13.deviantart.com/art/Starlight-Glimmer-Season-6-by-Zacatron94-678047433".to_string()),
                    author_name: Some("zacatron94".to_string()),
                    description: None,
                    images: vec![
                        ScrapeImage {
                            url: "https://derpicdn.net/img/view/2017/5/1/1426211__safe_artist-colon-zacatron94_starlight+glimmer_pony_unicorn_female_mare_simple+background_smiling_solo_transparent+background_vector.png".to_string(),
                            camo_url: "https://derpicdn.net/img/view/2017/5/1/1426211__safe_artist-colon-zacatron94_starlight+glimmer_pony_unicorn_female_mare_simple+background_smiling_solo_transparent+background_vector.png".to_string(),
                        },
                    ],
                },
            ),
            (
                r#"https://derpibooru.org/1426211"#,
                ScrapeResultData {
                    source_url: Some("http://brunomilan13.deviantart.com/art/Starlight-Glimmer-Season-6-by-Zacatron94-678047433".to_string()),
                    author_name: Some("zacatron94".to_string()),
                    description: None,
                    images: vec![
                        ScrapeImage {
                            url: "https://derpicdn.net/img/view/2017/5/1/1426211__safe_artist-colon-zacatron94_starlight+glimmer_pony_unicorn_female_mare_simple+background_smiling_solo_transparent+background_vector.png".to_string(),
                            camo_url: "https://derpicdn.net/img/view/2017/5/1/1426211__safe_artist-colon-zacatron94_starlight+glimmer_pony_unicorn_female_mare_simple+background_smiling_solo_transparent+background_vector.png".to_string(),
                        },
                    ],
                },
            ),
            (
                r#"https://derpibooru.org/images/1"#,
                ScrapeResultData {
                    source_url: Some("https://www.deviantart.com/speccysy/art/Afternoon-Flight-215193985".to_string()),
                    author_name: Some("speccysy".to_string()),
                    description: None,
                    images: vec![
                        ScrapeImage {
                            url: "https://derpicdn.net/img/view/2012/1/2/1__dead+source_safe_artist-colon-speccysy_fluttershy_pegasus_pony_g4_2011_artifact_cloud_cloudy_cute_derpibooru+legacy_eyes+closed_female_first+fluttershy.png".to_string(),
                            camo_url: "https://derpicdn.net/img/view/2012/1/2/1__dead+source_safe_artist-colon-speccysy_fluttershy_pegasus_pony_g4_2011_artifact_cloud_cloudy_cute_derpibooru+legacy_eyes+closed_female_first+fluttershy.png".to_string(),
                        },
                    ],
                },
            ),
            (
                r#"https://derpibooru.org/1"#,
                ScrapeResultData {
                    source_url: Some("https://www.deviantart.com/speccysy/art/Afternoon-Flight-215193985".to_string()),
                    author_name: Some("speccysy".to_string()),
                    description: None,
                    images: vec![
                        ScrapeImage {
                            url: "https://derpicdn.net/img/view/2012/1/2/1__dead+source_safe_artist-colon-speccysy_fluttershy_pegasus_pony_g4_2011_artifact_cloud_cloudy_cute_derpibooru+legacy_eyes+closed_female_first+fluttershy.png".to_string(),
                            camo_url: "https://derpicdn.net/img/view/2012/1/2/1__dead+source_safe_artist-colon-speccysy_fluttershy_pegasus_pony_g4_2011_artifact_cloud_cloudy_cute_derpibooru+legacy_eyes+closed_female_first+fluttershy.png".to_string(),
                        },
                    ],
                },
            ),
        ];
        let config = Configuration::default();
        let db = sled::Config::default().temporary(true).open()?;
        for url in urls {
            let scrape = tokio_test::block_on(scrape(&config, &db, url.0));
            let scrape = match scrape {
                Ok(s) => s,
                Err(e) => return Err(e),
            };
            let scrape = match scrape {
                Some(s) => s,
                None => anyhow::bail!("got none response from scraper"),
            };
            let expected_result = ScrapeResult::Ok(url.1);
            visit_diff::assert_eq_diff!(expected_result, scrape);
        }
        Ok(())
    }
}
