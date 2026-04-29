use clap::{Args, Command, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::exquis::midi::DeviceSelection;
use crate::exquis::proto::{Color, ColorCorrection, NamedZone};

/// Shared color-correction options for commands that send colors to the Exquis.
#[derive(Debug, Clone, Args)]
pub struct ColorCorrectionArgs {
    /// Gamma exponent applied to each channel (1.0 = identity; 2.2 darkens mids).
    #[arg(long, default_value_t = 1.0)]
    pub gamma: f32,
    /// Saturation multiplier (2.0 = default, compensates for Exquis LED desaturation).
    #[arg(long, default_value_t = 2.0)]
    pub saturation: f32,
    /// Per-channel brightness multiplier `r,g,b` (e.g. `1,1,0.5` to dim blue).
    #[arg(long, default_value = "1,1,1")]
    pub rgb_gain: String,
}

impl ColorCorrectionArgs {
    pub fn to_correction(&self) -> anyhow::Result<ColorCorrection> {
        let (r, g, b) = ColorCorrection::parse_rgb_gain(&self.rgb_gain)?;
        Ok(ColorCorrection {
            saturation: self.saturation,
            gamma: self.gamma,
            r_gain: r,
            g_gain: g,
            b_gain: b,
        })
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "xentool",
    version,
    about = "CLI for Intuitive Instruments Exquis MPE controllers and Wooting analog keyboards",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Explain commands and usage.
    Help { command: Option<String> },
    /// List detected Exquis MIDI devices.
    List,
    /// Monitor incoming MIDI/MPE data.
    Midi(MidiArgs),
    /// Toggle current developer mode zones.
    Dev {
        #[arg(value_enum)]
        action: DevAction,
        #[arg(long = "zone", value_enum, num_args = 0.., value_delimiter = ',')]
        zones: Vec<NamedZone>,
    },
    /// Set all pads at once.
    Pads(PadsArgs),
    /// Set an individual pad color (uses MPE-safe snapshot by default).
    Pad {
        pad: u8,
        color: Color,
        #[arg(long)]
        device: Option<DeviceSelection>,
        /// Use legacy dev-mode takeover instead of MPE-safe snapshot.
        #[arg(long)]
        legacy: bool,
    },
    /// Load .xtn layout, set colors, retune via pitch bend, and monitor.
    Serve {
        /// Path to the .xtn or .wtn layout file. If omitted, the last used layout
        /// (from settings.json `last_wtn`/`last_xtn`) is resumed if present.
        file: Option<std::path::PathBuf>,
        /// Pitch bend range in semitones (must match synth setting). Default: 16.
        /// Set the synth's per-note PB range to ±1600 cents to match.
        #[arg(long, default_value_t = 16.0)]
        pb_range: f64,
        /// Gain applied to the player's X-axis pitch-bend portion before
        /// recombining with the tuning offset. The Exquis caps physical X
        /// output at ~±170 LSBs of the 14-bit range (~2 %), so amplification
        /// is needed to make X audible. Default: 15 (full slide ≈ ±a perfect
        /// fourth — sized for Indian meend / Arabic ornament work). Tuning
        /// offset is never scaled — only player expression.
        #[arg(long, default_value_t = 15.0)]
        x_gain: f64,
        /// Output MIDI port name for pitch bend retuning. Default: "loopMIDI Port".
        #[arg(long, default_value = "loopMIDI Port")]
        output: String,
        /// Use MTS-ESP tuning instead of pitch bend retuning.
        #[arg(long)]
        mts_esp: bool,
        #[command(flatten)]
        color: ColorCorrectionArgs,
    },
    /// Create a new empty .xtn file for a given EDO and board count (all pads black/0/0).
    New {
        /// Output .xtn path (must not exist).
        file: std::path::PathBuf,
        /// EDO tuning (steps per octave).
        #[arg(long)]
        edo: i32,
        /// Number of boards. Default 4.
        #[arg(long, default_value_t = 4)]
        boards: u8,
        /// Pitch offset. Default 0.
        #[arg(long, default_value_t = 0)]
        pitch_offset: i32,
        /// Overwrite if the file already exists.
        #[arg(long)]
        force: bool,
    },
    /// Load an .xtn layout file and apply colors to connected boards.
    Load {
        /// Path to the .xtn layout file.
        file: std::path::PathBuf,
        #[command(flatten)]
        color: ColorCorrectionArgs,
    },
    /// List known hex-grid geometries (Exquis, Lumatone, Wooting).
    Geometries,
    /// Visualize one geometry as an SVG diagram of dots + neighbor connections.
    Geometry {
        /// Geometry name: `exquis`, `lumatone` (or `ltn`), `wooting` (or `wtn`).
        name: String,
        /// Number of Exquis boards to render (only used for `exquis`). Default 4.
        #[arg(long, default_value_t = 4)]
        boards: u8,
        /// Output SVG path. Default: a temp file that gets opened in the browser.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Do not auto-open the SVG in the browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Open a web-based visual editor for an .xtn layout file.
    Edit {
        /// Path to the .xtn layout file.
        file: std::path::PathBuf,
        /// HTTP port for the editor.
        #[arg(long, default_value_t = 8088)]
        port: u16,
        /// Do not auto-open the browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Set the color of a non-pad control (encoder, button, slider).
    Control {
        /// Control name or ID (e.g. "settings", "encoder-1", "110").
        control: String,
        /// Color to set.
        color: Color,
        #[arg(long, default_value = "all")]
        device: DeviceSelection,
    },
    /// Highlight MIDI notes on pads via channel 1 (works without dev mode).
    Highlight {
        /// MIDI note number to highlight (e.g. 60 for middle C)
        note: u8,
        /// Velocity (1-127 to turn on, 0 to turn off)
        #[arg(default_value_t = 127)]
        velocity: u8,
        #[arg(long, default_value = "all")]
        device: DeviceSelection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DevAction {
    On,
    Off,
}

#[derive(Debug, Clone, Args)]
pub struct MidiArgs {
    #[arg(long)]
    pub device: Option<DeviceSelection>,
    #[arg(long, value_enum, default_value_t = MidiMode::Hybrid)]
    pub mode: MidiMode,
    #[arg(long, help = "Show only note and X/Y/Z MPE events")]
    pub mpe_only: bool,
    #[arg(long)]
    pub no_log: bool,
    #[arg(long)]
    pub log_raw: bool,
    #[arg(long)]
    pub log_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MidiMode {
    Hybrid,
    Stream,
    Dashboard,
    Raw,
}

#[derive(Debug, Clone, Args)]
pub struct PadsArgs {
    #[command(subcommand)]
    pub command: PadsCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum PadsCommands {
    /// Set all pads black.
    Clear {
        #[arg(long, default_value = "all")]
        device: DeviceSelection,
        /// Use legacy dev-mode takeover instead of MPE-safe snapshot.
        #[arg(long)]
        legacy: bool,
    },
    /// Fill all pads with one color.
    Fill {
        color: Color,
        #[arg(long, default_value = "all")]
        device: DeviceSelection,
        /// Use legacy dev-mode takeover instead of MPE-safe snapshot.
        #[arg(long)]
        legacy: bool,
    },
    /// Write a simple test pattern.
    Test {
        #[arg(long, default_value = "all")]
        device: DeviceSelection,
        /// Use legacy dev-mode takeover instead of MPE-safe snapshot.
        #[arg(long)]
        legacy: bool,
    },
}

pub fn default_zone_mask() -> u8 {
    NamedZone::Pads.bit()
        | NamedZone::Encoders.bit()
        | NamedZone::Slider.bit()
        | NamedZone::UpDown.bit()
        | NamedZone::OtherButtons.bit()
}

impl Commands {
    pub fn list_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("list").unwrap().clone()
    }

    pub fn midi_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("midi").unwrap().clone()
    }

    pub fn dev_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("dev").unwrap().clone()
    }

    pub fn pads_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("pads").unwrap().clone()
    }

    pub fn pad_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("pad").unwrap().clone()
    }

    pub fn load_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("load").unwrap().clone()
    }

    pub fn control_command() -> Command {
        let cmd = Cli::command();
        cmd.find_subcommand("control").unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_pad_command() {
        let cli = Cli::parse_from(["xentool", "pad", "12", "amber", "--device", "2"]);
        match cli.command {
            Commands::Pad {
                pad,
                color,
                device,
                legacy,
            } => {
                assert_eq!(pad, 12);
                assert_eq!(color.red, 127);
                assert_eq!(device, Some(DeviceSelection::One(2)));
                assert!(!legacy);
            }
            _ => panic!("wrong command parsed"),
        }
    }

    #[test]
    fn parses_default_midi_mode() {
        let cli = Cli::parse_from(["xentool", "midi"]);
        match cli.command {
            Commands::Midi(args) => {
                assert_eq!(args.mode, MidiMode::Hybrid);
                assert!(!args.mpe_only);
            }
            _ => panic!("wrong command parsed"),
        }
    }
}
