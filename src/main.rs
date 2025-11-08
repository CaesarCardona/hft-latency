use std::io::{self, stdout};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use rand::Rng;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    symbols,
    widgets::{Block, Borders, Paragraph, Chart, Dataset, Axis},
    Terminal,
};

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

fn main() -> io::Result<()> {
    let n_stocks = 3;
    let colors = [Color::Red, Color::Green, Color::Yellow];

    // --- Backend data ---
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

    // --- Frontend data ---
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
        thread::spawn(move || {
            let mut rng = rand::thread_rng();
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
                    }
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
            // --- Split vertically: top pointers, bottom charts ---
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(8), Constraint::Min(10)])
                .split(f.area());

            // --- Pointer display ---
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

            // --- Split bottom horizontally: left = backend, right = frontend ---
            let chart_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(main_chunks[1]);

            // --- Backend chart ---
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
                .fold(f64::INFINITY, f64::min) - 1.0;
            let max_md = md_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max) + 1.0;

            let backend_chart = Chart::new(md_datasets)
                .block(Block::default().borders(Borders::ALL).title("Backend Stocks"))
                .x_axis(Axis::default().bounds([0.0, HISTORY_LEN as f64]))
                .y_axis(Axis::default().bounds([min_md, max_md]));

            f.render_widget(backend_chart, chart_chunks[0]);

            // --- Frontend chart ---
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
                .fold(f64::INFINITY, f64::min) - 1.0;
            let max_ui = ui_vec
                .iter()
                .flat_map(|x| x.history.iter())
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max) + 1.0;

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

