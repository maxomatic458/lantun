#![allow(dead_code)]

use lantun_core::LanTun;
use ratatui::{DefaultTerminal, Frame};
use std::{io, time::Instant};

pub struct TuiApp {
    lantun: LanTun,
    exit: bool,
    selected_tunnel: usize,
    num_host_tunnels: usize,
    num_client_tunnels: usize,
    last_input_time: Instant,
}

impl TuiApp {
    pub fn new(lantun: LanTun) -> Self {
        let num_host_tunnels = lantun.host.tunnels().len();
        let num_client_tunnels = lantun.client.tunnels().len();

        Self {
            lantun,
            exit: false,
            selected_tunnel: 0,
            num_host_tunnels,
            num_client_tunnels,
            last_input_time: Instant::now(),
        }
    }

    pub fn run(&mut self, _terminal: &mut DefaultTerminal) -> io::Result<()> {
        Ok(())
    }

    fn draw(&self, _frame: &mut Frame) {}

    fn handle_events(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn start_tui(mut state: TuiApp) -> color_eyre::Result<()> {
    let mut terminal = ratatui::init();
    let res = state.run(&mut terminal);
    res?;
    ratatui::restore();
    Ok(())
}
