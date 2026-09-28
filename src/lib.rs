use std::error::Error;
use std::fmt::{self, Display};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use clap::ValueEnum;
use flate2::{Compression, GzBuilder};
use tiff::ColorType;
use tiff::decoder::{ChunkType, Decoder, DecodingResult};
use tiff::tags::Tag;

pub const MAGIC: i32 = 0x0108_AAFF;
pub const NO_DATA: i32 = i32::MIN;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum Rotation {
    #[default]
    #[value(name = "0")]
    Deg0,
    #[value(name = "90")]
    Deg90,
    #[value(name = "180")]
    Deg180,
    #[value(name = "270")]
    Deg270,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Transform {
    pub rotation: Rotation,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

impl Transform {
    fn output_dimensions(self, width: u32, height: u32) -> (u32, u32) {
        match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => (width, height),
            Rotation::Deg90 | Rotation::Deg270 => (height, width),
        }
    }

    fn map(self, x: u32, y: u32, width: u32, height: u32) -> (u32, u32) {
        let (output_width, output_height) = self.output_dimensions(width, height);
        let (mut output_x, mut output_y) = match self.rotation {
            Rotation::Deg0 => (x, y),
            Rotation::Deg90 => (height - 1 - y, x),
            Rotation::Deg180 => (width - 1 - x, height - 1 - y),
            Rotation::Deg270 => (y, width - 1 - x),
        };

        if self.flip_horizontal {
            output_x = output_width - 1 - output_x;
        }
        if self.flip_vertical {
            output_y = output_height - 1 - output_y;
        }
        (output_x, output_y)
    }
}

#[derive(Debug)]
struct MessageError(String);

impl Display for MessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for MessageError {}

fn error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(MessageError(message.into()))
}

struct RasterInfo {
    width: u32,
    height: u32,
    no_data: Option<i64>,
    chunk_type: ChunkType,
    chunk_width: u32,
    chunk_height: u32,
    chunk_count: u32,
}

pub fn convert(input: &Path, output: &Path, transform: Transform) -> Result<()> {
    let (info, minimum, maximum) = inspect_raster(input)?;
    let (output_width, output_height) = validate_dimensions(info.width, info.height, transform)?;
    let payload_size = decoded_payload_size(output_width, output_height)?;
    let mut scratch = tempfile::tempfile()?;
    scratch.set_len(payload_size)?;

    let input_file = File::open(input)?;
    let mut decoder = Decoder::new(BufReader::new(input_file))?;
    {
        let mut scratch_writer = BufWriter::new(&mut scratch);
        for chunk_index in 0..info.chunk_count {
            let values = signed_values(decoder.read_chunk(chunk_index)?)?;
            let (chunk_width, chunk_height) = decoder.chunk_data_dimensions(chunk_index);
            let (chunk_x, chunk_y) = chunk_origin(&info, chunk_index);
            write_transformed_chunk(
                &mut scratch_writer,
                &values,
                chunk_x,
                chunk_y,
                chunk_width,
                chunk_height,
                &info,
                transform,
                minimum,
                maximum,
            )?;
        }
        scratch_writer.flush()?;
    }
    scratch.seek(SeekFrom::Start(0))?;

    let output_file = File::create(output).map_err(|source| {
        error(format!(
            "failed to create output {}: {source}",
            output.display()
        ))
    })?;
    let mut output_writer = BufWriter::new(output_file);
    write_header(&mut output_writer, output_width, output_height)?;
    let mut gzip_writer = GzBuilder::new()
        .mtime(0)
        .write(output_writer, Compression::default());
    let bytes_written = io::copy(&mut scratch, &mut gzip_writer)?;
    if bytes_written != payload_size {
        return Err(error("temporary payload was truncated before compression"));
    }
    output_writer = gzip_writer.finish()?;
    output_writer.flush()?;
    Ok(())
}

fn inspect_raster(path: &Path) -> Result<(RasterInfo, i64, i64)> {
    let file = File::open(path)
        .map_err(|source| error(format!("failed to open input {}: {source}", path.display())))?;
    let mut decoder = Decoder::new(BufReader::new(file))?;
    let (width, height) = decoder.dimensions()?;
    validate_source_type(&mut decoder)?;
    if decoder.more_images() {
        return Err(error("input must contain exactly one image"));
    }

    let no_data = read_no_data(&mut decoder)?;
    let chunk_type = decoder.get_chunk_type();
    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    let chunk_count = match chunk_type {
        ChunkType::Strip => decoder.strip_count()?,
        ChunkType::Tile => decoder.tile_count()?,
    };

    let mut minimum = None;
    let mut maximum = None;
    for chunk_index in 0..chunk_count {
        for value in signed_values(decoder.read_chunk(chunk_index)?)? {
            if Some(value) == no_data {
                continue;
            }
            minimum = Some(minimum.map_or(value, |current: i64| current.min(value)));
            maximum = Some(maximum.map_or(value, |current: i64| current.max(value)));
        }
    }

    let minimum = minimum.ok_or_else(|| error("input contains no valid elevation pixels"))?;
    let maximum = maximum.expect("minimum and maximum are populated together");
    Ok((
        RasterInfo {
            width,
            height,
            no_data,
            chunk_type,
            chunk_width,
            chunk_height,
            chunk_count,
        },
        minimum,
        maximum,
    ))
}

fn validate_source_type<R: std::io::Read + Seek>(decoder: &mut Decoder<R>) -> Result<()> {
    let samples_per_pixel = decoder
        .find_tag_unsigned::<u16>(Tag::SamplesPerPixel)?
        .unwrap_or(1);
    if samples_per_pixel != 1 {
        return Err(error(format!(
            "unsupported TIFF: expected one elevation band, found {samples_per_pixel} samples per pixel"
        )));
    }

    let sample_format = decoder
        .find_tag_unsigned::<u16>(Tag::SampleFormat)?
        .unwrap_or(1);
    let color_type = decoder.colortype()?;
    if sample_format != 2 || !matches!(color_type, ColorType::Gray(8 | 16 | 32 | 64)) {
        return Err(error(format!(
            "unsupported TIFF sample format: expected single-band signed integer, found {color_type:?} with sample format {sample_format}"
        )));
    }
    Ok(())
}

fn read_no_data<R: std::io::Read + Seek>(decoder: &mut Decoder<R>) -> Result<Option<i64>> {
    let Some(value) = decoder.find_tag(Tag::Unknown(42113))? else {
        return Ok(None);
    };
    let text = value.into_string()?;
    let text = text.trim_end_matches('\0').trim();
    let value = text.parse::<i64>().map_err(|_| {
        error(format!(
            "unsupported no-data value {text:?}: expected a signed integer"
        ))
    })?;
    Ok(Some(value))
}

fn signed_values(result: DecodingResult) -> Result<Vec<i64>> {
    let values = match result {
        DecodingResult::I8(values) => values.into_iter().map(i64::from).collect(),
        DecodingResult::I16(values) => values.into_iter().map(i64::from).collect(),
        DecodingResult::I32(values) => values.into_iter().map(i64::from).collect(),
        DecodingResult::I64(values) => values,
        other => {
            return Err(error(format!(
                "unsupported decoded sample buffer: {other:?}"
            )));
        }
    };
    Ok(values)
}

fn validate_dimensions(width: u32, height: u32, transform: Transform) -> Result<(u32, u32)> {
    let (output_width, output_height) = transform.output_dimensions(width, height);
    if output_width == 0 || output_height == 0 {
        return Err(error("raster dimensions must be positive"));
    }
    if output_width > i32::MAX as u32 || output_height > i32::MAX as u32 {
        return Err(error("raster dimensions exceed positive int32 range"));
    }
    decoded_payload_size(output_width, output_height)?;
    Ok((output_width, output_height))
}

fn decoded_payload_size(width: u32, height: u32) -> Result<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| error("decoded heightmap payload size overflows u64"))
}

fn write_header<W: Write>(writer: &mut W, width: u32, height: u32) -> Result<()> {
    writer.write_all(&MAGIC.to_le_bytes())?;
    writer.write_all(&(height as i32).to_le_bytes())?;
    writer.write_all(&(width as i32).to_le_bytes())?;
    Ok(())
}

fn chunk_origin(info: &RasterInfo, chunk_index: u32) -> (u32, u32) {
    match info.chunk_type {
        ChunkType::Strip => (0, chunk_index * info.chunk_height),
        ChunkType::Tile => {
            let chunks_across = info.width.div_ceil(info.chunk_width);
            (
                (chunk_index % chunks_across) * info.chunk_width,
                (chunk_index / chunks_across) * info.chunk_height,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_transformed_chunk<W: Write + Seek>(
    writer: &mut W,
    values: &[i64],
    chunk_x: u32,
    chunk_y: u32,
    chunk_width: u32,
    chunk_height: u32,
    info: &RasterInfo,
    transform: Transform,
    minimum: i64,
    maximum: i64,
) -> Result<()> {
    match transform.rotation {
        Rotation::Deg0 | Rotation::Deg180 => {
            for local_y in 0..chunk_height {
                let start = (local_y * chunk_width) as usize;
                let end = start + chunk_width as usize;
                let mut line =
                    normalize_values(&values[start..end], info.no_data, minimum, maximum);
                write_mapped_line(
                    writer,
                    &mut line,
                    chunk_x,
                    chunk_y + local_y,
                    chunk_x + chunk_width - 1,
                    chunk_y + local_y,
                    info,
                    transform,
                )?;
            }
        }
        Rotation::Deg90 | Rotation::Deg270 => {
            for local_x in 0..chunk_width {
                let mut line = Vec::with_capacity(chunk_height as usize);
                for local_y in 0..chunk_height {
                    let index = (local_y * chunk_width + local_x) as usize;
                    line.push(normalize(values[index], info.no_data, minimum, maximum));
                }
                write_mapped_line(
                    writer,
                    &mut line,
                    chunk_x + local_x,
                    chunk_y,
                    chunk_x + local_x,
                    chunk_y + chunk_height - 1,
                    info,
                    transform,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_mapped_line<W: Write + Seek>(
    writer: &mut W,
    values: &mut [i32],
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
    info: &RasterInfo,
    transform: Transform,
) -> Result<()> {
    let (first_x, first_y) = transform.map(start_x, start_y, info.width, info.height);
    let (last_x, last_y) = transform.map(end_x, end_y, info.width, info.height);
    debug_assert_eq!(first_y, last_y);
    if first_x > last_x {
        values.reverse();
    }
    let output_x = first_x.min(last_x);
    let (output_width, _) = transform.output_dimensions(info.width, info.height);
    let sample_index = u64::from(first_y) * u64::from(output_width) + u64::from(output_x);
    let byte_offset = sample_index
        .checked_mul(4)
        .ok_or_else(|| error("output offset overflow"))?;
    writer.seek(SeekFrom::Start(byte_offset))?;
    for value in values {
        writer.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

fn normalize_values(values: &[i64], no_data: Option<i64>, minimum: i64, maximum: i64) -> Vec<i32> {
    values
        .iter()
        .map(|&value| normalize(value, no_data, minimum, maximum))
        .collect()
}

fn normalize(value: i64, no_data: Option<i64>, minimum: i64, maximum: i64) -> i32 {
    if Some(value) == no_data {
        return NO_DATA;
    }
    if minimum == maximum {
        return 0;
    }
    let offset = i128::from(value) - i128::from(minimum);
    let span = i128::from(maximum) - i128::from(minimum);
    let numerator = offset * i128::from(i32::MAX);
    ((numerator + span / 2) / span) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::fs;
    use std::io::{Cursor, Read};
    use tempfile::tempdir;
    use tiff::encoder::{TiffEncoder, colortype};

    fn write_fixture(path: &Path, width: u32, height: u32, values: &[i16], no_data: i16) {
        let file = File::create(path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder
            .new_image::<colortype::GrayI16>(width, height)
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(42113), no_data.to_string().as_str())
            .unwrap();
        image.write_data(values).unwrap();
    }

    fn read_heightmap(path: &Path) -> (i32, i32, i32, Vec<i32>) {
        let mut bytes = Vec::new();
        File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
        let header: [u8; 12] = bytes[..12].try_into().unwrap();
        let mut payload = Vec::new();
        GzDecoder::new(&bytes[12..])
            .read_to_end(&mut payload)
            .unwrap();
        let (word_bytes, remainder) = payload.as_chunks::<4>();
        assert!(remainder.is_empty());
        let words: Vec<i32> = word_bytes
            .iter()
            .map(|bytes| i32::from_le_bytes(*bytes))
            .collect();
        (
            i32::from_le_bytes(header[0..4].try_into().unwrap()),
            i32::from_le_bytes(header[4..8].try_into().unwrap()),
            i32::from_le_bytes(header[8..12].try_into().unwrap()),
            words,
        )
    }

    #[test]
    fn converts_exact_header_normalization_no_data_and_row_order() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let output = directory.path().join("small.hm");
        write_fixture(&input, 3, 2, &[0, 10, -999, 20, 30, 40], -999);

        convert(&input, &output, Transform::default()).unwrap();

        assert_eq!(
            read_heightmap(&output),
            (
                MAGIC,
                2,
                3,
                vec![
                    0,
                    536_870_912,
                    NO_DATA,
                    1_073_741_824,
                    1_610_612_735,
                    i32::MAX
                ]
            )
        );
        let output_bytes = fs::read(output).unwrap();
        assert_eq!(&output_bytes[12..14], &[0x1f, 0x8b]);
    }

    #[test]
    fn rotates_clockwise_then_flips_in_output_coordinates() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let output = directory.path().join("small.hm");
        write_fixture(&input, 3, 2, &[0, 1, 2, 3, 4, 5], -999);
        let transform = Transform {
            rotation: Rotation::Deg90,
            flip_horizontal: true,
            flip_vertical: false,
        };

        convert(&input, &output, transform).unwrap();

        assert_eq!(
            read_heightmap(&output),
            (
                MAGIC,
                3,
                2,
                vec![
                    0,
                    1_288_490_188,
                    429_496_729,
                    1_717_986_918,
                    858_993_459,
                    i32::MAX
                ]
            )
        );
    }

    #[test]
    fn maps_all_rotations_and_flips() {
        let dimensions = (3, 2);
        assert_eq!(
            Transform {
                rotation: Rotation::Deg90,
                ..Transform::default()
            }
            .map(0, 0, dimensions.0, dimensions.1),
            (1, 0)
        );
        assert_eq!(
            Transform {
                rotation: Rotation::Deg180,
                ..Transform::default()
            }
            .map(0, 0, dimensions.0, dimensions.1),
            (2, 1)
        );
        assert_eq!(
            Transform {
                rotation: Rotation::Deg270,
                ..Transform::default()
            }
            .map(0, 0, dimensions.0, dimensions.1),
            (0, 2)
        );
        assert_eq!(
            Transform {
                flip_horizontal: true,
                ..Transform::default()
            }
            .map(0, 0, dimensions.0, dimensions.1),
            (2, 0)
        );
        assert_eq!(
            Transform {
                flip_vertical: true,
                ..Transform::default()
            }
            .map(0, 0, dimensions.0, dimensions.1),
            (0, 1)
        );
    }

    #[test]
    fn maps_constant_elevation_to_zero() {
        assert_eq!(normalize(7, None, 7, 7), 0);
        assert_eq!(normalize(-1, Some(-1), 7, 7), NO_DATA);
    }

    #[test]
    fn rounds_with_wide_intermediates() {
        assert_eq!(normalize(i64::MIN, None, i64::MIN, i64::MAX), 0);
        assert_eq!(normalize(i64::MAX, None, i64::MIN, i64::MAX), i32::MAX);
    }

    #[test]
    fn rejects_dimensions_and_size_overflow() {
        assert!(validate_dimensions(0, 1, Transform::default()).is_err());
        assert!(validate_dimensions(i32::MAX as u32 + 1, 1, Transform::default()).is_err());
        assert!(decoded_payload_size(u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn writes_little_endian_header_bytes() {
        let mut bytes = Cursor::new(Vec::new());
        write_header(&mut bytes, 3, 2).unwrap();
        assert_eq!(
            bytes.into_inner(),
            [0xff, 0xaa, 0x08, 0x01, 2, 0, 0, 0, 3, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_unsigned_samples() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("unsigned.tif");
        let output = directory.path().join("unsigned.hm");
        let file = File::create(&input).unwrap();
        TiffEncoder::new(file)
            .unwrap()
            .write_image::<colortype::Gray16>(1, 1, &[42])
            .unwrap();

        let message = convert(&input, &output, Transform::default())
            .unwrap_err()
            .to_string();
        assert!(message.contains("single-band signed integer"), "{message}");
    }

    #[test]
    fn rejects_malformed_tiff() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("malformed.tif");
        let output = directory.path().join("malformed.hm");
        fs::write(&input, b"not a TIFF").unwrap();

        assert!(convert(&input, &output, Transform::default()).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn gzip_output_is_deterministic() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let first = directory.path().join("first.hm");
        let second = directory.path().join("second.hm");
        write_fixture(&input, 3, 2, &[0, 10, -999, 20, 30, 40], -999);

        convert(&input, &first, Transform::default()).unwrap();
        convert(&input, &second, Transform::default()).unwrap();

        assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
    }
}
