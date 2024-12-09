use actix_files::Files;
use actix_multipart::Multipart;
use actix_web::{error::ErrorInternalServerError, web, App, Error, HttpResponse, HttpServer};
use chrono::Utc;
use futures_util::stream::StreamExt as _;
use sanitize_filename::sanitize;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::env;

const MAIN_PAGE_TITLE: &str = "All Articles";

#[derive(Serialize, Deserialize)]
struct CommentForm {
    comment: String,
}

#[derive(Serialize, FromRow)]
struct DbArticle {
    id: i32,
    title: String,
    body: String,
    bump_time: i64,
}

#[derive(Serialize)]
struct Article {
    id: i32,
    title: String,
    body: String,
    media_paths: Vec<String>,
    bump_time: i64,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    create_and_set_permissions("uploads")?;

    // Retrieve DATABASE_URL from environment
    let database_url = env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set in the environment before running");

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to Postgres");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .route("/", web::get().to(new_article_form))
            .route("/submit", web::post().to(submit_article))
            .route("/articles", web::get().to(list_articles))
            .route("/articles/{id}", web::get().to(view_article))
            .route("/articles/{id}/comment", web::post().to(submit_comment))
            .service(Files::new("/static", "./static"))
            .service(Files::new("/uploads", "./uploads"))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}

// Function to create a directory and set permissions
fn create_and_set_permissions(dir: &str) -> std::io::Result<()> {
    if !Path::new(dir).exists() {
        fs::create_dir(dir)?;
    }
    Ok(())
}

// Utility function to log errors to "error.txt"
fn log_error(error_message: &str) {
    if let Ok(file) = OpenOptions::new().create(true).append(true).open("error.txt") {
        let mut writer = BufWriter::new(file);
        let _ = writeln!(writer, "ERROR: {}", error_message);
    }
}

// Route to display the article submission form
async fn new_article_form() -> HttpResponse {
    let html = format!(
        r#"
    <!DOCTYPE html>
    <html lang="en">
    <head>
        <meta charset="UTF-8">
        <title>Submit a New Article</title>
        <link rel="stylesheet" href="/static/style.css">
        <style>
            .post-form-box {{
                background: #fff;
                padding: 20px;
                border-radius: 8px;
                box-shadow: 0 0 10px rgba(0, 0, 0, 0.1);
                margin: 50px auto;
                max-width: 400px;
                text-align: center;
            }}
            form input[type="text"], form textarea {{
                width: 100%;
                padding: 10px;
                margin-top: 10px;
                margin-bottom: 15px;
                border: 1px solid #ccc;
                border-radius: 4px;
                box-sizing: border-box;
            }}
            form input[type="file"] {{
                margin-bottom: 15px;
            }}
            form input[type="submit"] {{
                background: #333;
                color: #fff;
                padding: 10px 20px;
                border: none;
                border-radius: 4px;
                cursor: pointer;
            }}
            form input[type="submit"]:hover {{
                background: #555;
            }}
        </style>
    </head>
    <body>
        <div class="post-form-box">
            <h1>Submit a New Article</h1>
            <form action="/submit" method="POST" enctype="multipart/form-data">
                <input type="text" name="title" placeholder="Title" required><br>
                <textarea name="body" rows="10" placeholder="Body" required></textarea><br>
                <input type="file" name="media" accept=".jpg,.jpeg,.png,.gif,.webp,.mp4" required><br><br>
                <label>jpg, png, gif, webp, or MP4</label><br><br>
                <input type="submit" value="Submit Article">
            </form>
        </div>
        <br>
        <a href="/articles" style="display: block; text-align: center;">View All Articles</a>
    </body>
    </html>
    "#
    );

    HttpResponse::Ok().content_type("text/html").body(html)
}

// Handle submission of new articles
async fn submit_article(
    pool: web::Data<PgPool>,
    mut payload: Multipart,
) -> Result<HttpResponse, Error> {
    let mut title = String::new();
    let mut body = String::new();
    let mut media_paths = Vec::new();

    create_and_set_permissions("uploads").expect("Failed to create or set permissions for uploads directory");

    while let Some(item) = payload.next().await {
        let mut field = item?;
        let content_disposition = field.content_disposition().unwrap();
        let field_name = content_disposition.get_name().unwrap();

        if field_name == "title" {
            let mut value = Vec::new();
            while let Some(chunk) = field.next().await {
                value.extend_from_slice(&chunk?);
            }
            title = String::from_utf8(value).unwrap_or_default();
        } else if field_name == "body" {
            let mut value = Vec::new();
            while let Some(chunk) = field.next().await {
                value.extend_from_slice(&chunk?);
            }
            body = String::from_utf8(value).unwrap_or_default();
        } else if field_name == "media" {
            if let Some(filename) = content_disposition.get_filename() {
                let sanitized_filename = sanitize(&filename);
                let filepath = format!("./uploads/article_{}", sanitized_filename);
                let mut f = File::create(&filepath)
                    .map_err(|e| ErrorInternalServerError(format!("Failed to create file: {}", e)))?;
                while let Some(chunk) = field.next().await {
                    f.write_all(&chunk?)
                        .map_err(|e| ErrorInternalServerError(format!("Failed to write file: {}", e)))?;
                }
                media_paths.push(format!("/uploads/article_{}", sanitized_filename));
            }
        }
    }

    if media_paths.is_empty() {
        return Ok(HttpResponse::BadRequest().body("Media file is required"));
    }

    let bump_time = Utc::now().timestamp();

    let article_id: i32 = sqlx::query_scalar(
        "INSERT INTO articles (title, body, bump_time) VALUES ($1, $2, $3) RETURNING id"
    )
    .bind(&title)
    .bind(&body)
    .bind(bump_time)
    .fetch_one(pool.get_ref())
    .await
    .map_err(|e| {
        log_error(&format!("Failed to store article: {}", e));
        ErrorInternalServerError("Database insert failed")
    })?;

    // Insert media
    for path in media_paths {
        sqlx::query("INSERT INTO article_media (article_id, media_path) VALUES ($1, $2)")
            .bind(article_id)
            .bind(path)
            .execute(pool.get_ref())
            .await
            .map_err(|e| {
                log_error(&format!("Failed to store media: {}", e));
                ErrorInternalServerError("Failed to store media")
            })?;
    }

    Ok(HttpResponse::Found()
        .append_header(("Location", "/articles"))
        .finish())
}

// List all articles
async fn list_articles(pool: web::Data<PgPool>) -> HttpResponse {
    let articles_db = match sqlx::query_as::<_, DbArticle>("SELECT id, title, body, bump_time FROM articles ORDER BY bump_time DESC")
        .fetch_all(pool.get_ref())
        .await {
            Ok(a) => a,
            Err(e) => {
                log_error(&format!("Failed to fetch articles: {}", e));
                return HttpResponse::InternalServerError().body("Failed to load articles");
            }
        };

    let mut articles_html = format!(r#"
    <!DOCTYPE html>
    <html lang="en">
    <head>
        <meta charset="UTF-8">
        <title>{}</title>
        <link rel="stylesheet" href="/static/style.css">
    </head>
    <body>
        <h1>{}</h1>
        <div style="text-align: center; margin-bottom: 20px;">
            <a href="/">Submit a New Article</a>
        </div>
    "#, MAIN_PAGE_TITLE, MAIN_PAGE_TITLE);

    for article in &articles_db {
        articles_html.push_str(&format!(
            r#"<div class="article-link">
            <h2><a href="/articles/{}">{}</a></h2>
            </div>"#,
            article.id, article.title
        ));
    }

    articles_html.push_str("</body></html>");

    HttpResponse::Ok().content_type("text/html").body(articles_html)
}

// View an article by ID
async fn view_article(pool: web::Data<PgPool>, path: web::Path<i32>) -> HttpResponse {
    let article_id = path.into_inner();

    let article_db = match sqlx::query_as::<_, DbArticle>(
        "SELECT id, title, body, bump_time FROM articles WHERE id = $1",
    )
    .bind(article_id)
    .fetch_one(pool.get_ref())
    .await
    {
        Ok(a) => a,
        Err(_) => return HttpResponse::NotFound().body("Article not found"),
    };

    let media_paths = sqlx::query!("SELECT media_path FROM article_media WHERE article_id = $1", article_db.id)
        .fetch_all(pool.get_ref())
        .await
        .map(|rows| rows.into_iter().map(|r| r.media_path).collect::<Vec<_>>())
        .unwrap_or_default();

    let article = Article {
        id: article_db.id,
        title: article_db.title,
        body: article_db.body,
        bump_time: article_db.bump_time,
        media_paths,
    };

    let comments = sqlx::query!("SELECT comment FROM comments WHERE article_id = $1", article.id)
        .fetch_all(pool.get_ref())
        .await
        .map(|rows| rows.into_iter().map(|r| r.comment).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut article_html = String::new();
    article_html.push_str(r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8">"#);
    article_html.push_str(&format!("<title>{}</title>", article.title));
    article_html.push_str(r#"<link rel="stylesheet" href="/static/style.css"></head><body>"#);
    article_html.push_str(
        r#"<div style="text-align: center; margin-bottom: 20px;"><a href="/articles">← Back to All Articles</a></div>"#,
    );
    article_html.push_str(&format!("<h1>{}</h1>", article.title));

    for media in &article.media_paths {
        if media.ends_with(".mp4") {
            article_html.push_str(&format!(
                r#"<video controls width="600">
                    <source src="{}" type="video/mp4">
                    Your browser does not support the video tag.
                </video><br>"#,
                media
            ));
        } else {
            article_html.push_str(&format!(
                r#"<img src="{}" alt="Article Image" style="max-width: 100%; height: auto;"><br>"#,
                media
            ));
        }
    }

    article_html.push_str(&format!(
        r#"
        <p>{}</p>
        <h3>Leave a Comment</h3>
        <form action="/articles/{}/comment" method="POST">
            <textarea name="comment" rows="4" required></textarea><br>
            <input type="submit" value="Submit Comment">
        </form>
        <h3>Comments</h3>
    "#,
        article.body, article.id
    ));

    for comment in comments {
        article_html.push_str(&format!(r#"<div class="comment"><p>{}</p></div>"#, comment));
    }

    article_html.push_str("</body></html>");

    HttpResponse::Ok().content_type("text/html").body(article_html)
}

// Submit comment
async fn submit_comment(
    pool: web::Data<PgPool>,
    path: web::Path<i32>,
    form: web::Form<CommentForm>,
) -> HttpResponse {
    let article_id = path.into_inner();

    if let Err(e) = sqlx::query("INSERT INTO comments (article_id, comment) VALUES ($1, $2)")
        .bind(article_id)
        .bind(&form.comment)
        .execute(pool.get_ref())
        .await
    {
        log_error(&format!("Failed to store comment: {}", e));
        return HttpResponse::InternalServerError().body("Failed to store comment.");
    }

    let new_bump_time = Utc::now().timestamp();
    if let Err(e) = sqlx::query("UPDATE articles SET bump_time = $1 WHERE id = $2")
        .bind(new_bump_time)
        .bind(article_id)
        .execute(pool.get_ref())
        .await
    {
        log_error(&format!("Failed to bump article: {}", e));
        return HttpResponse::InternalServerError().body("Failed to bump article.");
    }

    HttpResponse::Found()
        .append_header(("Location", format!("/articles/{}", article_id)))
        .finish()
}
