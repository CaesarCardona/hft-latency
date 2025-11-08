use std::fs::{self, OpenOptions};
use std::io::{self, stdout, Write};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use log::{info, error};
use rand::Rng;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    symbols,
    widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph},
    Terminal,
};
use redis::AsyncCommands;
use sqlx::postgres::PgPoolOptions;

const HISTORY_LEN: usize = 50;
const MOVING_AVG_LEN: usize = 5;

#[derive(Clone)]
struct MarketData {
    count: usize,
    price: Arc<RwLock<f64>>,
    last_update: Instant,
    history: Vec<f64>,
}

#[derive(Clone)]
struct UiData {
    count: usize,
    value: Arc<f64>,
    last_update: Instant,
    history: Vec<f64>,
}

// -------------------- Helper functions --------------------

fn append_to_file(stock_id: i32, price: f64) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("stock_data.txt")?;
    writeln!(file, "{},{}", stock_id, price)?;
    Ok(())
}

async fn flush_file_to_postgres(pool: Arc<sqlx::PgPool>) -> std::io::Result<()> {
    let content = fs::read_to_string("stock_data.txt")?;
    if content.is_empty() {
        return Ok(());
    }

    info!("Flushing {} lines to Postgres...", content.lines().count());

    for line in content.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 2 { continue; }

        let stock_id: i32 = match parts[0].parse() {
            Ok(n) => n,
            Err(_) => { error!("Failed to parse stock_id: {}", parts[0]); continue; }
        };
        let price: f32 = match parts[1].parse() {
            Ok(p) => p,
            Err(_) => { error!("Failed to parse price: {}", parts[1]); continue; }
        };

        if let Err(e) = sqlx::query(
            "INSERT INTO stock_data (stock_id, price, ts) VALUES ($1, $2, NOW())"
        )
        .bind(stock_id)
        .bind(price)
        .execute(&*pool)
        .await
        {
            error!("Postgres insert error: {:?}", e);
        }
    }

    fs::File::create("stock_data.txt")?;
    info!("Flushed stock_data.txt to Postgres successfully.");
    Ok(())
}

fn init_logging() {
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Stdout)
        .init();
}

// -------------------- Main --------------------

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    init_logging();

    let n_stocks = 3;
    let colors = [Color::Red, Color::Green, Color::Yellow];

    // --- Postgres pool ---
    let pg_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect("postgres://postgres:test@localhost/hft")
        .await
        .expect("Failed to connect to Postgres");
    let pg_pool = Arc::new(pg_pool);

    // --- Redis client ---
    let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();
    let redis_client = Arc::new(redis_client);

    // --- Market data ---
    let market_data = Arc::new(RwLock::new(
        (0..n_stocks)
            .map(|i| {
                let init = 100.0;
                MarketData {
                    count: i,
                    price: Arc::new(RwLock::new(init)),
                    last_update: Instant::now(),
                    history: vec![init; HISTORY_LEN],
                }
            })
            .collect::<Vec<_>>(),
    ));

    // --- UI data ---
    let ui_data = Arc::new(RwLock::new(
        (0..n_stocks)
            .map(|i| UiData {
                count: i,
                value: Arc::new(100.0),
                last_update: Instant::now(),
                history: vec![],
            })
            .collect::<Vec<_>>(),
    ));

    // --- Backend updater thread ---
    {
        let md_clone = Arc::clone(&market_data);
        let pg_pool = Arc::clone(&pg_pool);
        let redis_client = Arc::clone(&redis_client);

        thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let rt = tokio::runtime::Runtime::new().unwrap();
            let flush_interval = Duration::from_secs(1);
            let mut last_flush = Instant::now();

            loop {
                {
                    let mut vec = md_clone.write().unwrap();
                    for md in vec.iter_mut() {
                        let delta = rng.gen_range(-2.0..2.0);
                        let mut p = md.price.write().unwrap();
                        *p += delta;
                        md.last_update = Instant::now();
                        md.history.push(*p);
                        if md.history.len() > HISTORY_LEN {
                            md.history.remove(0);
                        }

                        let stock_id = md.count as i32;
                        let price_f64 = *p;

                        let _ = append_to_file(stock_id, price_f64);

                        let redis_client = Arc::clone(&redis_client);
                        rt.spawn(async move {
                            if let Ok(mut conn) = redis_client.get_async_connection().await {
                                let _: () = conn
                                    .set(format!("stock:{}", stock_id), price_f64 as f32)
                                    .await
                                    .unwrap_or(());
                            }
                        });
                    }
                }

                // Flush to Postgres every second
                if last_flush.elapsed() >= flush_interval {
                    let pool_clone = Arc::clone(&pg_pool);
                    if let Err(e) = rt.block_on(flush_file_to_postgres(pool_clone)) {
                        error!("Flush failed: {:?}", e);
                    }
                    last_flush = Instant::now();
                }

                thread::sleep(Duration::from_millis(100));
            }
        });
    }

    // --- Frontend updater thread (moving average) ---
    {
        let md_clone = Arc::clone(&market_data);
        let ui_clone = Arc::clone(&ui_data);

        thread::spawn(move || {
            loop {
                {
                    let md_vec = md_clone.read().unwrap();
                    let mut ui_vec = ui_clone.write().unwrap();
                    for (i, ui) in ui_vec.iter_mut().enumerate() {
                        let len = md_vec[i].history.len();
                        let start = len.saturating_sub(MOVING_AVG_LEN);
                        let slice = &md_vec[i].history[start..];
                        let avg = slice.iter().sum::<f64>() / slice.len() as f64;

                        let new_ptr = Arc::new(avg);
                        ui.value = new_ptr.clone();
                        ui.last_update = Instant::now();
                        ui.history.push(avg);
                        if ui.history.len() > HISTORY_LEN {
                            ui.history.remove(0);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(300));
            }
        });
    }

    // --- Terminal setup ---
    enable_raw_mode()?;
    let mut stdout = stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // --- Main loop ---
    loop {
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }

        let md_vec = market_data.read().unwrap().clone();
        let ui_vec = ui_data.read().unwrap().clone();

        terminal.draw(|f| {
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(8), Constraint::Min(10)])
                .split(f.area());

            // --- Pointers ---
            let mut lines = vec![];
            for md in md_vec.iter() {
                let val = *md.price.read().unwrap();
                lines.push(ratatui::text::Line::from(format!(
                    "Backend Stock {} -> ptr: {:p}, value: {:.2}",
                    md.count,
                    Arc::as_ptr(&md.price),
                    val
                )));
            }
            for ui in ui_vec.iter() {
                lines.push(ratatui::text::Line::from(format!(
                    "Frontend Stock {} -> ptr: {:p}, moving avg: {:.2}",
                    ui.count,
                    Arc::as_ptr(&ui.value),
                    *ui.value
                )));
            }
            f.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Pointers")),
                main_chunks[0],
            );

            // --- Charts ---
            let chart_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(main_chunks[1]);

            // Backend chart
            let md_points: Vec<Vec<(f64, f64)>> = md_vec
                .iter()
                .map(|md| md.history.iter().enumerate().map(|(i, y)| (i as f64, *y)).collect())
                .collect();

            let md_datasets: Vec<Dataset> = md_points
                .iter()
                .enumerate()
                .map(|(i, pts)| {
                    Dataset::default()
                        .name(format!("Backend {}", i))
                        .marker(symbols::Marker::Dot)
                        .style(Style::default().fg(colors[i]))
                        .data(pts)
                })
                .collect();

            let min_md = md_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::INFINITY, f64::min)
                - 1.0;
            let max_md = md_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max)
                + 1.0;

            let backend_chart = Chart::new(md_datasets)
                .block(Block::default().borders(Borders::ALL).title("Backend Stocks"))
                .x_axis(Axis::default().bounds([0.0, HISTORY_LEN as f64]))
                .y_axis(Axis::default().bounds([min_md, max_md]));

            f.render_widget(backend_chart, chart_chunks[0]);

            // Frontend chart
            let ui_points: Vec<Vec<(f64, f64)>> = ui_vec
                .iter()
                .map(|ui| ui.history.iter().enumerate().map(|(i, y)| (i as f64, *y)).collect())
                .collect();

            let ui_datasets: Vec<Dataset> = ui_points
                .iter()
                .enumerate()
                .map(|(i, pts)| {
                    Dataset::default()
                        .name(format!("Frontend {}", i))
                        .marker(symbols::Marker::Braille)
                        .style(Style::default().fg(colors[i]))
                        .data(pts)
                })
                .collect();

            let min_ui = ui_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::INFINITY, f64::min)
                - 1.0;
            let max_ui = ui_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max)
                + 1.0;

            let frontend_chart = Chart::new(ui_datasets)
                .block(Block::default().borders(Borders::ALL).title("Frontend Moving Avg"))
                .x_axis(Axis::default().bounds([0.0, HISTORY_LEN as f64]))
                .y_axis(Axis::default().bounds([min_ui, max_ui]));

            f.render_widget(frontend_chart, chart_chunks[1]);
        })?;

        thread::sleep(Duration::from_millis(50));
    }

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

