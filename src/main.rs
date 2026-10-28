use dotenv::dotenv;
use reqwest::Client;
use std::collections::{HashMap, HashSet};

mod get_language_color;
use get_language_color::load_language_colors;

mod draw_svg;
// use draw_svg::generate_svg;
use draw_svg::generate_compact_svg;

mod forge;
use clap::Parser;
use forge::{fetch_all_repos, fetch_languages, get_username, Forge};

#[derive(Parser, Debug)]
#[command(name = "self-reposcope", about = "GitHub/Forgejo Language Stats Generator")]
struct Args {
    /// GitHub Token
    #[clap(short, long, env = "GITHUB_TOKEN")]
    token: Option<String>,

    /// Forgejo instance URL, e.g. https://git.example.com
    #[clap(long, env = "FORGEJO_URL", requires = "forgejo_token")]
    forgejo_url: Option<String>,

    /// Forgejo access token (read:repository + read:user)
    #[clap(long, env = "FORGEJO_TOKEN", requires = "forgejo_url")]
    forgejo_token: Option<String>,

    /// Comma-separated string-list of languages to exclude
    #[clap(short, long, env = "EXCLUDED_LANGUAGES", default_value = "")]
    excluded_languages: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let args = Args::parse();

    let mut forges: Vec<Forge> = vec![];
    if let Some(token) = args.token.filter(|t| !t.is_empty()) {
        forges.push(Forge::github(token));
    }
    if let (Some(url), Some(token)) = (args.forgejo_url, args.forgejo_token) {
        if !url.is_empty() && !token.is_empty() {
            forges.push(Forge::forgejo(&url, token));
        }
    }
    if forges.is_empty() {
        return Err("no forge configured: set --token and/or --forgejo-url + --forgejo-token".into());
    }

    let excluded_languages: Vec<String> = args
        .excluded_languages
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let client = Client::new();
    let mut total_lang_map: HashMap<String, u64> = HashMap::new();
    // A repo mirrored on several forges must only be counted once (first forge wins).
    let mut seen_repos: HashSet<String> = HashSet::new();

    for forge in &forges {
        let user_name = get_username(&client, forge).await?;
        let repos = fetch_all_repos(&client, forge).await?;
        println!("[{}] Found {} repositories:", forge.label(), repos.len());

        for repo in repos {
            if repo.fork {
                continue;
            }

            if repo.owner.owner_type.as_deref() == Some("Organization") {
                continue; // Organization所有のリポジトリ除外
            }

            if repo.owner.login != user_name {
                continue;
            }

            if !seen_repos.insert(repo.name.to_lowercase()) {
                println!(" - {} (already counted from another forge, skipped)", repo.name);
                continue;
            }

            println!(" - {}", repo.name);

            let lang_map = match fetch_languages(&client, forge, &repo.owner.login, &repo.name).await {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Failed to fetch languages for {}: {}", repo.name, e);
                    continue;
                }
            };

            for (lang, bytes) in lang_map {
                if excluded_languages.iter().any(|excl| excl.eq_ignore_ascii_case(&lang)) {
                    continue;
                }
                *total_lang_map.entry(lang).or_insert(0) += bytes;
            }
        }
    }
    println!("------");

    let mut lang_vec: Vec<(String, u64)> = total_lang_map.into_iter().collect();
    lang_vec.sort_by(|a, b| b.1.cmp(&a.1));

    println!("\n=== Aggregated Language Usage ===");
    for (i, (lang, bytes)) in lang_vec.iter().enumerate() {
        println!(" {}. {}: {} bytes", i + 1, lang, bytes);
    }

    let color_map = load_language_colors();
    // generate_svg(&lang_vec, &color_map, "output/language_chart.svg")?;
    generate_compact_svg(&lang_vec, &color_map, "output/full_languages.svg")?;

    Ok(())
}
