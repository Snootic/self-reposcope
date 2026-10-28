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
    /// `api_base` is `https://api.github.com` for github.com, or a GitHub Enterprise API root.
    pub fn github(api_base: &str, token: String) -> Self {
        Self {
            kind: Kind::GitHub,
            api_base: api_base.trim_end_matches('/').to_string(),
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
        self.request(client.get(url))
    }

    fn post(&self, client: &Client, url: &str) -> RequestBuilder {
        self.request(client.post(url))
    }

    fn request(&self, req: RequestBuilder) -> RequestBuilder {
        let req = req.header("User-Agent", "self-reposcope");
        match self.kind {
            Kind::GitHub => req.bearer_auth(&self.token),
            Kind::Forgejo => req.header("Authorization", format!("token {}", self.token)),
        }
    }
}

/// Languages of one repository, in bytes.
pub struct RepoLangs {
    pub name: String,
    pub languages: HashMap<String, u64>,
}

/// Everything one forge contributes to the aggregate.
pub struct ForgeStats {
    pub repos: Vec<RepoLangs>,
    /// Language colors reported by the forge (GitHub only); may be empty.
    pub colors: HashMap<String, String>,
}

type BoxError = Box<dyn std::error::Error>;

pub async fn fetch_stats(client: &Client, forge: &Forge) -> Result<ForgeStats, BoxError> {
    match forge.kind {
        Kind::GitHub => fetch_github(client, forge).await,
        Kind::Forgejo => fetch_forgejo(client, forge).await,
    }
}

// ---------------------------------------------------------------- GitHub (GraphQL)

// Own, non-fork repositories with their languages in one round trip per 50 repos.
// `ownerAffiliations: OWNER` excludes organization repositories.
const GITHUB_QUERY: &str = r#"
query($after: String) {
  viewer {
    repositories(first: 50, after: $after, ownerAffiliations: OWNER, isFork: false) {
      pageInfo { hasNextPage endCursor }
      nodes {
        name
        languages(first: 100, orderBy: {field: SIZE, direction: DESC}) {
          edges { size node { name color } }
        }
      }
    }
  }
}"#;

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct GqlData {
    viewer: GqlViewer,
}

#[derive(Deserialize)]
struct GqlViewer {
    repositories: GqlRepos,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRepos {
    page_info: GqlPageInfo,
    nodes: Vec<GqlRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct GqlRepo {
    name: String,
    languages: GqlLanguages,
}

#[derive(Deserialize)]
struct GqlLanguages {
    edges: Vec<GqlEdge>,
}

#[derive(Deserialize)]
struct GqlEdge {
    size: u64,
    node: GqlLang,
}

#[derive(Deserialize)]
struct GqlLang {
    name: String,
    color: Option<String>,
}

async fn fetch_github(client: &Client, forge: &Forge) -> Result<ForgeStats, BoxError> {
    let url = format!("{}/graphql", forge.api_base);
    let mut stats = ForgeStats {
        repos: vec![],
        colors: HashMap::new(),
    };
    let mut after: Option<String> = None;

    loop {
        let body = serde_json::json!({ "query": GITHUB_QUERY, "variables": { "after": after } });
        let resp: GqlResponse = forge
            .post(client, &url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if !resp.errors.is_empty() {
            let msgs: Vec<_> = resp.errors.iter().map(|e| e.message.as_str()).collect();
            return Err(format!("GitHub GraphQL error: {}", msgs.join("; ")).into());
        }
        let repos = resp.data.ok_or("GitHub GraphQL returned no data")?.viewer.repositories;

        for repo in repos.nodes {
            let mut languages = HashMap::new();
            for edge in repo.languages.edges {
                if let Some(color) = edge.node.color {
                    stats.colors.insert(edge.node.name.clone(), color);
                }
                languages.insert(edge.node.name, edge.size);
            }
            stats.repos.push(RepoLangs { name: repo.name, languages });
        }

        match (repos.page_info.has_next_page, repos.page_info.end_cursor) {
            (true, Some(cursor)) => after = Some(cursor),
            _ => break,
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------- Forgejo (REST)

#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Deserialize)]
struct Repo {
    name: String,
    fork: bool,
    owner: Owner,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

// Forgejo has no GraphQL and no bulk language endpoint: one request per repo.
async fn fetch_forgejo(client: &Client, forge: &Forge) -> Result<ForgeStats, BoxError> {
    let user: User = forge
        .get(client, &format!("{}/user", forge.api_base))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut stats = ForgeStats {
        repos: vec![],
        colors: HashMap::new(),
    };
    let mut page = 1;

    loop {
        // Forgejo pages with `limit` (server caps it, default max 50).
        let url = format!("{}/user/repos?limit=50&page={}", forge.api_base, page);
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
        page += 1;

        for repo in batch {
            // Skip forks and repos owned by organizations (owner differs from the token's user).
            if repo.fork || repo.owner.login != user.login {
                continue;
            }
            let url = format!(
                "{}/repos/{}/{}/languages",
                forge.api_base, repo.owner.login, repo.name
            );
            let res = forge.get(client, &url).send().await?;
            if !res.status().is_success() {
                eprintln!("Failed to fetch languages for {}: {}", repo.name, res.status());
                continue;
            }
            stats.repos.push(RepoLangs {
                name: repo.name,
                languages: res.json().await?,
            });
        }
    }

    Ok(stats)
}
