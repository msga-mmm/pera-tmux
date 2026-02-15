use pera_tmux::{Event, Tmux};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmux = Tmux::connect().await?;

    let focused = tmux.focused_pane().await?;
    println!("{focused:?}");

    tmux.on(Event::PaneFocusIn, |pane| {
        println!("Focused pane changed: {:?}", pane);
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}
