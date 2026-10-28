use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use std::collections::HashMap;

/// Supported forge flavours. Forgejo/Gitea share the same REST API shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    GitHub,
    Forgejo,
}

/// One configured forge account to aggregate from.
#[derive(Debug, Clone)]
pub struct Forge {
    pub kind: Kind,
    /// API root without trailing slash, e.g. `https://api.github.com` or `https://git.example.com/api/v1`.
    pub api_base: String,
    pub token: String,
}

impl Forge {
    pub fn github(token: String) -> Self {
        Self {
            kind: Kind::GitHub,
            api_base: "https://api.github.com".to_string(),
            token,
        }
    }

    /// `url` is the instance root (e.g. `https://git.example.com`); `/api/v1` is appended.
    pub fn forgejo(url: &str, token: String) -> Self {
        Self {
            kind: Kind::Forgejo,
            api_base: format!("{}/api/v1", url.trim_end_matches('/')),
            token,
        }
    }

    pub fn label(&self) -> &str {
        match self.kind {
            Kind::GitHub => "GitHub",
            Kind::Forgejo => "Forgejo",
        }
    }

    fn get(&self, client: &Client, url: &str) -> RequestBuilder {
        let req = client.get(url).header("User-Agent", "self-reposcope");
        match self.kind {
            Kind::GitHub => req.bearer_auth(&self.token),
            Kind::Forgejo => req.header("Authorization", format!("token {}", self.token)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Owner {
    pub login: String,
    /// GitHub only ("User" / "Organization"); Forgejo omits it.
    #[serde(rename = "type", default)]
    pub owner_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Repo {
    pub name: String,
    pub fork: bool,
    pub owner: Owner,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

pub async fn get_username(client: &Client, forge: &Forge) -> Result<String, reqwest::Error> {
    let url = format!("{}/user", forge.api_base);
    let user: User = forge
        .get(client, &url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(user.login)
}

pub async fn fetch_all_repos(client: &Client, forge: &Forge) -> Result<Vec<Repo>, reqwest::Error> {
    let mut repos = vec![];
    let mut page = 1;

    loop {
        // GitHub pages with `per_page` (max 100); Forgejo with `limit` (server caps it, default max 50).
        let url = match forge.kind {
            Kind::GitHub => format!("{}/user/repos?per_page=100&page={}", forge.api_base, page),
            Kind::Forgejo => format!("{}/user/repos?limit=50&page={}", forge.api_base, page),
        };
        let batch: Vec<Repo> = forge
            .get(client, &url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if batch.is_empty() {
            break;
        }

        repos.extend(batch);
        page += 1;
    }

    Ok(repos)
}

/// Bytes per language for one repo. Same endpoint shape on GitHub and Forgejo.
pub async fn fetch_languages(
    client: &Client,
    forge: &Forge,
    owner: &str,
    repo: &str,
) -> Result<HashMap<String, u64>, reqwest::Error> {
    let url = format!("{}/repos/{}/{}/languages", forge.api_base, owner, repo);
    forge
        .get(client, &url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}
