use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dem2hm::{Rotation, Transform, convert};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Convert a signed-integer GeoTIFF DEM to a compact heightmap"
)]
struct Arguments {
    /// Input single-band signed-integer GeoTIFF
    input: PathBuf,

    /// Output heightmap file
    output: PathBuf,

    /// Clockwise rotation in degrees
    #[arg(long, value_enum, default_value_t)]
    rotate: Rotation,

    /// Flip the rotated raster left-to-right
    #[arg(long)]
    flip_horizontal: bool,

    /// Flip the rotated raster top-to-bottom
    #[arg(long)]
    flip_vertical: bool,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let transform = Transform {
        rotation: arguments.rotate,
        flip_horizontal: arguments.flip_horizontal,
        flip_vertical: arguments.flip_vertical,
    };

    match convert(&arguments.input, &arguments.output, transform) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dem2hm: {error}");
            ExitCode::FAILURE
        }
    }
}
