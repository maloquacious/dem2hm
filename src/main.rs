use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dem2hm::{HeightMap16Options, Rotation, Transform, write_heightmap, write_heightmap16};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum Format {
    /// Normalized int32 pixels (frozen version 1)
    #[value(name = "1")]
    V1,
    /// int16 meters with georeference and provenance metadata
    #[default]
    #[value(name = "1.1")]
    V1_1,
}

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

    /// Heightmap format version to write
    #[arg(long, value_enum, default_value_t)]
    format: Format,

    /// Clockwise rotation in degrees
    #[arg(long, value_enum, default_value_t)]
    rotate: Rotation,

    /// Flip the rotated raster left-to-right
    #[arg(long)]
    flip_horizontal: bool,

    /// Flip the rotated raster top-to-bottom
    #[arg(long)]
    flip_vertical: bool,

    /// Vertical datum to record when the GeoTIFF does not declare one (format 1.1 only)
    #[arg(long, value_name = "NAME")]
    vertical_datum: Option<String>,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let transform = Transform {
        rotation: arguments.rotate,
        flip_horizontal: arguments.flip_horizontal,
        flip_vertical: arguments.flip_vertical,
    };

    let result = match arguments.format {
        Format::V1 if arguments.vertical_datum.is_some() => {
            Err("--vertical-datum requires --format 1.1".into())
        }
        Format::V1 => write_heightmap(&arguments.input, &arguments.output, transform),
        Format::V1_1 => write_heightmap16(
            &arguments.input,
            &arguments.output,
            &HeightMap16Options {
                transform,
                vertical_datum: arguments.vertical_datum,
            },
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dem2hm: {error}");
            ExitCode::FAILURE
        }
    }
}
