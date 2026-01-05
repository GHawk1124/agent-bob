mod ui;
mod web;

use std::error::Error;

fn handle(input: &str) -> String {
    format!("User message: {}", input)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let queries = vec![
        "rust tokio JoinSet example".to_string(),
        "html2md rust convert html to markdown".to_string(),
    ];

    let results_per_query = 3;
    let pages = web::search(&queries, results_per_query).await?;

    for p in pages {
        println!("\n==============================");
        println!("Query:  {}", p.query);
        println!("Title:  {}", p.title.as_deref().unwrap_or("(none)"));
        println!("URL:    {}", p.url);
        println!("Status: {}", p.status);
        println!("------------------------------\n");
        println!("{}", p.markdown);
    }

    Ok(())
    // ui::run(handle)
}
