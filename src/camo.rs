use crate::Configuration;
use anyhow::Result;
use camo_url::CamoConfig;
use url::Url;

pub fn camo_url(config: &Configuration, url: &Url) -> Result<Url> {
    let camo_key = if let Some(key) = config.camo_key.clone() {
        key
    } else {
        return Ok(url.clone());
    };
    let camo_key = hex::encode(camo_key);
    let camo_host = if let Some(host) = config.camo_host.clone() {
        host
    } else {
        return Ok(url.clone());
    };
    let camo = CamoConfig::new(camo_key, camo_host);
    let camo = match camo {
        Err(e) => anyhow::bail!(format!("camo config invalid: {}", e)),
        Ok(camo) => camo,
    };
    let url = camo.get_camo_url(url);
    match url {
        Ok(url) => Ok(url),
        Err(e) => anyhow::bail!(format!("camo url invalid: {}", e)),
    }
}
