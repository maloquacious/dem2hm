use std::error::Error;
use std::fmt::{self, Display};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use clap::ValueEnum;
use flate2::{Compression, GzBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tiff::ColorType;
use tiff::decoder::{ChunkType, Decoder, DecodingResult};
use tiff::tags::Tag;

mod geotiff;

/// Magic value of a version 1 heightmap of normalized `int32` pixels.
pub const MAGIC: i32 = 0x0108_AAFF;
/// Version 1 no-data sentinel.
pub const NO_DATA: i32 = i32::MIN;
/// Magic value of a version 1.1 heightmap of `int16` elevations in meters.
pub const MAGIC16: i32 = 0x0108_AAFE;
/// Version 1.1 no-data sentinel: the pixel lies outside the source data.
pub const NO_DATA16: i16 = i16::MIN;
/// Largest version 1.1 JSON metadata block in bytes.
pub const MAX_METADATA_SIZE: usize = 1 << 20;

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

impl Rotation {
    fn degrees(self) -> u16 {
        match self {
            Rotation::Deg0 => 0,
            Rotation::Deg90 => 90,
            Rotation::Deg180 => 180,
            Rotation::Deg270 => 270,
        }
    }
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

/// Settings for writing a version 1.1 heightmap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HeightMap16Options {
    pub transform: Transform,
    /// Vertical datum to record when the GeoTIFF keys do not declare one.
    pub vertical_datum: Option<String>,
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

/// Version 1.1 JSON metadata. Field order is the serialized key order.
#[derive(Debug, Serialize)]
struct Metadata {
    height: u32,
    width: u32,
    elevation: ElevationMetadata,
    pixel_size_m: f64,
    source: SourceMetadata,
    transform: TransformMetadata,
    dem2hm_version: &'static str,
}

#[derive(Debug, Serialize)]
struct ElevationMetadata {
    minimum: i16,
    maximum: i16,
    vertical_datum: Option<String>,
}

#[derive(Debug, Serialize)]
struct SourceMetadata {
    file_name: String,
    sha256: String,
    width: u32,
    height: u32,
    no_data: Option<i64>,
    geotransform: [f64; 6],
}

#[derive(Debug, Serialize)]
struct TransformMetadata {
    rotate: u16,
    flip_horizontal: bool,
    flip_vertical: bool,
}

/// Converts `input` to a version 1 heightmap. It forwards to [`write_heightmap`].
pub fn convert(input: &Path, output: &Path, transform: Transform) -> Result<()> {
    write_heightmap(input, output, transform)
}

/// Converts `input` to a version 1 heightmap of normalized `int32` pixels.
pub fn write_heightmap(input: &Path, output: &Path, transform: Transform) -> Result<()> {
    let (info, minimum, maximum) = inspect_raster(input)?;
    let (output_width, output_height) = validate_dimensions(info.width, info.height, transform)?;
    let payload_size = decoded_payload_size(output_width, output_height, 4)?;
    let no_data = info.no_data;
    let scratch = write_payload(input, &info, transform, payload_size, |value| {
        Ok(normalize(value, no_data, minimum, maximum).to_le_bytes())
    })?;

    let mut header = Vec::new();
    write_header(&mut header, output_width, output_height)?;
    write_output(output, &header, scratch, payload_size)
}

/// Converts `input` to a version 1.1 heightmap of `int16` elevations in meters
/// with georeference and provenance metadata.
pub fn write_heightmap16(input: &Path, output: &Path, options: &HeightMap16Options) -> Result<()> {
    let (info, minimum, maximum) = inspect_raster(input)?;
    let (minimum, maximum) = (elevation16(minimum)?, elevation16(maximum)?);
    let georeference = read_georeference(input)?;
    let transform = options.transform;
    let (output_width, output_height) = validate_dimensions(info.width, info.height, transform)?;
    let payload_size = decoded_payload_size(output_width, output_height, 2)?;

    let metadata = Metadata {
        height: output_height,
        width: output_width,
        elevation: ElevationMetadata {
            minimum,
            maximum,
            vertical_datum: georeference
                .vertical_datum
                .or_else(|| options.vertical_datum.clone()),
        },
        pixel_size_m: geotiff::nominal_pixel_size_m(&georeference.geotransform, info.height)?,
        source: SourceMetadata {
            file_name: input
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            sha256: sha256_file(input)?,
            width: info.width,
            height: info.height,
            no_data: info.no_data,
            geotransform: georeference.geotransform,
        },
        transform: TransformMetadata {
            rotate: transform.rotation.degrees(),
            flip_horizontal: transform.flip_horizontal,
            flip_vertical: transform.flip_vertical,
        },
        dem2hm_version: env!("CARGO_PKG_VERSION"),
    };

    let no_data = info.no_data;
    let scratch = write_payload(input, &info, transform, payload_size, |value| {
        if Some(value) == no_data {
            return Ok(NO_DATA16.to_le_bytes());
        }
        Ok(elevation16(value)?.to_le_bytes())
    })?;

    let mut header = Vec::new();
    write_header16(&mut header, &metadata)?;
    write_output(output, &header, scratch, payload_size)
}

/// Decodes, transforms and encodes every source pixel into a temporary
/// uncompressed payload of `N`-byte samples.
fn write_payload<const N: usize>(
    input: &Path,
    info: &RasterInfo,
    transform: Transform,
    payload_size: u64,
    encode: impl Fn(i64) -> Result<[u8; N]>,
) -> Result<File> {
    let mut scratch = tempfile::tempfile()?;
    scratch.set_len(payload_size)?;

    let input_file = File::open(input)?;
    let mut decoder = Decoder::new(BufReader::new(input_file))?;
    {
        let mut scratch_writer = BufWriter::new(&mut scratch);
        for chunk_index in 0..info.chunk_count {
            let values = signed_values(decoder.read_chunk(chunk_index)?)?;
            let (chunk_width, chunk_height) = decoder.chunk_data_dimensions(chunk_index);
            let (chunk_x, chunk_y) = chunk_origin(info, chunk_index);
            write_transformed_chunk(
                &mut scratch_writer,
                &values,
                chunk_x,
                chunk_y,
                chunk_width,
                chunk_height,
                info,
                transform,
                &encode,
            )?;
        }
        scratch_writer.flush()?;
    }
    scratch.seek(SeekFrom::Start(0))?;
    Ok(scratch)
}

/// Writes `header` followed by `payload` compressed as one deterministic gzip member.
fn write_output(output: &Path, header: &[u8], mut payload: File, payload_size: u64) -> Result<()> {
    let output_file = File::create(output).map_err(|source| {
        error(format!(
            "failed to create output {}: {source}",
            output.display()
        ))
    })?;
    let mut output_writer = BufWriter::new(output_file);
    output_writer.write_all(header)?;
    let mut gzip_writer = GzBuilder::new()
        .mtime(0)
        .write(output_writer, Compression::default());
    let bytes_written = io::copy(&mut payload, &mut gzip_writer)?;
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

fn read_georeference(path: &Path) -> Result<geotiff::Georeference> {
    let file = File::open(path)
        .map_err(|source| error(format!("failed to open input {}: {source}", path.display())))?;
    let mut decoder = Decoder::new(BufReader::new(file))?;
    geotiff::read_georeference(&mut decoder)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|source| error(format!("failed to open input {}: {source}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
    decoded_payload_size(output_width, output_height, 4)?;
    Ok((output_width, output_height))
}

fn decoded_payload_size(width: u32, height: u32, sample_size: u64) -> Result<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(sample_size))
        .ok_or_else(|| error("decoded heightmap payload size overflows u64"))
}

fn write_header<W: Write>(writer: &mut W, width: u32, height: u32) -> Result<()> {
    writer.write_all(&MAGIC.to_le_bytes())?;
    writer.write_all(&(height as i32).to_le_bytes())?;
    writer.write_all(&(width as i32).to_le_bytes())?;
    Ok(())
}

/// Writes the version 1.1 magic value, the metadata length and the metadata.
fn write_header16<W: Write>(writer: &mut W, metadata: &Metadata) -> Result<()> {
    let json = serde_json::to_vec(metadata)?;
    if json.len() > MAX_METADATA_SIZE {
        return Err(error(format!(
            "metadata is {} bytes, exceeding the {MAX_METADATA_SIZE}-byte limit",
            json.len()
        )));
    }
    writer.write_all(&MAGIC16.to_le_bytes())?;
    writer.write_all(&i32::try_from(json.len())?.to_le_bytes())?;
    writer.write_all(&json)?;
    Ok(())
}

/// Converts a valid elevation to `int16` meters, rejecting values that do not
/// fit or that collide with the no-data sentinel.
fn elevation16(value: i64) -> Result<i16> {
    match i16::try_from(value) {
        Ok(elevation) if elevation != NO_DATA16 => Ok(elevation),
        _ => Err(error(format!(
            "valid elevation {value} is outside the int16 range {}..={} (-32768 is reserved for no-data)",
            i16::MIN + 1,
            i16::MAX
        ))),
    }
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
fn write_transformed_chunk<const N: usize, W: Write + Seek>(
    writer: &mut W,
    values: &[i64],
    chunk_x: u32,
    chunk_y: u32,
    chunk_width: u32,
    chunk_height: u32,
    info: &RasterInfo,
    transform: Transform,
    encode: &impl Fn(i64) -> Result<[u8; N]>,
) -> Result<()> {
    match transform.rotation {
        Rotation::Deg0 | Rotation::Deg180 => {
            for local_y in 0..chunk_height {
                let start = (local_y * chunk_width) as usize;
                let end = start + chunk_width as usize;
                let mut line = values[start..end]
                    .iter()
                    .map(|&value| encode(value))
                    .collect::<Result<Vec<_>>>()?;
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
                    line.push(encode(values[index])?);
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
fn write_mapped_line<const N: usize, W: Write + Seek>(
    writer: &mut W,
    samples: &mut [[u8; N]],
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
        samples.reverse();
    }
    let output_x = first_x.min(last_x);
    let (output_width, _) = transform.output_dimensions(info.width, info.height);
    let sample_index = u64::from(first_y) * u64::from(output_width) + u64::from(output_x);
    let byte_offset = sample_index
        .checked_mul(N as u64)
        .ok_or_else(|| error("output offset overflow"))?;
    writer.seek(SeekFrom::Start(byte_offset))?;
    writer.write_all(samples.as_flattened())?;
    Ok(())
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
    use tiff::encoder::{DirectoryEncoder, TiffEncoder, TiffKind, colortype};

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
        assert!(decoded_payload_size(u32::MAX, u32::MAX, 4).is_err());
        assert!(decoded_payload_size(u32::MAX, u32::MAX, 2).is_err());
        assert!(decoded_payload_size(i32::MAX as u32, i32::MAX as u32, 4).is_ok());
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

    const GEOGRAPHIC_KEYS: [u16; 12] = [1024, 0, 1, 2, 1025, 0, 1, 1, 2054, 0, 1, 9102];

    struct GeoFixture<'a> {
        keys: &'a [u16],
        ascii: Option<&'a str>,
        no_data: Option<i64>,
    }

    impl Default for GeoFixture<'_> {
        fn default() -> Self {
            Self {
                keys: &GEOGRAPHIC_KEYS,
                ascii: None,
                no_data: Some(-999),
            }
        }
    }

    impl GeoFixture<'_> {
        fn write_tags<W: Write + Seek, K: TiffKind>(
            &self,
            encoder: &mut DirectoryEncoder<'_, W, K>,
        ) {
            // Top-left corner at 80°W 9°N; pixels are 0.5° wide and 0.25° tall.
            encoder
                .write_tag(Tag::Unknown(33922), &[0.0, 0.0, 0.0, -80.0, 9.0, 0.0][..])
                .unwrap();
            encoder
                .write_tag(Tag::Unknown(33550), &[0.5, 0.25, 0.0][..])
                .unwrap();
            let mut directory = vec![1, 1, 0, (self.keys.len() / 4) as u16];
            directory.extend_from_slice(self.keys);
            encoder
                .write_tag(Tag::Unknown(34735), &directory[..])
                .unwrap();
            if let Some(ascii) = self.ascii {
                encoder.write_tag(Tag::Unknown(34737), ascii).unwrap();
            }
            if let Some(no_data) = self.no_data {
                encoder
                    .write_tag(Tag::Unknown(42113), no_data.to_string().as_str())
                    .unwrap();
            }
        }

        fn write_i16(&self, path: &Path, width: u32, height: u32, values: &[i16]) {
            let mut encoder = TiffEncoder::new(File::create(path).unwrap()).unwrap();
            let mut image = encoder
                .new_image::<colortype::GrayI16>(width, height)
                .unwrap();
            self.write_tags(image.encoder());
            image.write_data(values).unwrap();
        }

        fn write_i32(&self, path: &Path, width: u32, height: u32, values: &[i32]) {
            let mut encoder = TiffEncoder::new(File::create(path).unwrap()).unwrap();
            let mut image = encoder
                .new_image::<colortype::GrayI32>(width, height)
                .unwrap();
            self.write_tags(image.encoder());
            image.write_data(values).unwrap();
        }
    }

    fn read_heightmap16(path: &Path) -> (Vec<u8>, serde_json::Value, Vec<i16>) {
        let bytes = fs::read(path).unwrap();
        let length = i32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let json = &bytes[8..8 + length];
        let mut payload = Vec::new();
        GzDecoder::new(&bytes[8 + length..])
            .read_to_end(&mut payload)
            .unwrap();
        let (sample_bytes, remainder) = payload.as_chunks::<2>();
        assert!(remainder.is_empty());
        (
            bytes[..8 + length].to_vec(),
            serde_json::from_slice(json).unwrap(),
            sample_bytes
                .iter()
                .map(|bytes| i16::from_le_bytes(*bytes))
                .collect(),
        )
    }

    #[test]
    fn writes_heightmap16_header_metadata_no_data_and_row_order() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let output = directory.path().join("small.hmz");
        GeoFixture::default().write_i16(&input, 3, 2, &[-37, 0, -999, 20, 3431, 40]);

        write_heightmap16(&input, &output, &HeightMap16Options::default()).unwrap();

        let (header, metadata, pixels) = read_heightmap16(&output);
        assert_eq!(&header[0..4], &[0xfe, 0xaa, 0x08, 0x01]);
        assert_eq!(
            i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize,
            header.len() - 8
        );
        let json = std::str::from_utf8(&header[8..]).unwrap();
        assert!(
            json.starts_with(r#"{"height":2,"width":3,"elevation":{"minimum":-37,"maximum":3431,"#),
            "{json}"
        );
        let output_bytes = fs::read(&output).unwrap();
        assert_eq!(&output_bytes[header.len()..header.len() + 2], &[0x1f, 0x8b]);
        assert_eq!(pixels, vec![-37, 0, NO_DATA16, 20, 3431, 40]);

        let pixel_size = metadata["pixel_size_m"].as_f64().unwrap();
        assert!((pixel_size - 27_650.0).abs() < 10.0, "{pixel_size}");
        assert_eq!(
            metadata,
            serde_json::json!({
                "height": 2,
                "width": 3,
                "elevation": {"minimum": -37, "maximum": 3431, "vertical_datum": null},
                "pixel_size_m": pixel_size,
                "source": {
                    "file_name": "small.tif",
                    "sha256": sha256_file(&input).unwrap(),
                    "width": 3,
                    "height": 2,
                    "no_data": -999,
                    "geotransform": [-80.0, 0.5, 0.0, 9.0, 0.0, -0.25],
                },
                "transform": {"rotate": 0, "flip_horizontal": false, "flip_vertical": false},
                "dem2hm_version": env!("CARGO_PKG_VERSION"),
            })
        );
    }

    #[test]
    fn heightmap16_records_rotation_and_flips() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let output = directory.path().join("small.hmz");
        GeoFixture::default().write_i16(&input, 3, 2, &[0, 1, 2, 3, 4, 5]);
        let options = HeightMap16Options {
            transform: Transform {
                rotation: Rotation::Deg90,
                flip_horizontal: true,
                flip_vertical: false,
            },
            vertical_datum: None,
        };

        write_heightmap16(&input, &output, &options).unwrap();

        let (_, metadata, pixels) = read_heightmap16(&output);
        assert_eq!(
            (metadata["height"].as_i64(), metadata["width"].as_i64()),
            (Some(3), Some(2))
        );
        assert_eq!(
            metadata["transform"],
            serde_json::json!({"rotate": 90, "flip_horizontal": true, "flip_vertical": false})
        );
        assert_eq!(pixels, vec![0, 3, 1, 4, 2, 5]);
    }

    #[test]
    fn heightmap16_accepts_wide_sources_that_fit() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("wide.tif");
        let output = directory.path().join("wide.hmz");
        GeoFixture::default().write_i32(&input, 2, 1, &[-32767, 32767]);

        write_heightmap16(&input, &output, &HeightMap16Options::default()).unwrap();

        assert_eq!(read_heightmap16(&output).2, vec![-32767, 32767]);
    }

    #[test]
    fn heightmap16_rejects_out_of_range_elevations() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("out.hmz");
        for (name, values) in [
            ("high", [0, 32768]),
            ("low", [0, -32769]),
            ("sentinel", [0, -32768]),
        ] {
            let input = directory.path().join(format!("{name}.tif"));
            GeoFixture::default().write_i32(&input, 2, 1, &values);

            let message = write_heightmap16(&input, &output, &HeightMap16Options::default())
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("outside the int16 range"),
                "{name}: {message}"
            );
            assert!(!output.exists(), "{name}");
        }
    }

    #[test]
    fn heightmap16_maps_source_no_data_even_when_outside_int16() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("no_data.tif");
        let output = directory.path().join("no_data.hmz");
        let fixture = GeoFixture {
            no_data: Some(-100_000),
            ..GeoFixture::default()
        };
        fixture.write_i32(&input, 2, 1, &[-100_000, 5]);

        write_heightmap16(&input, &output, &HeightMap16Options::default()).unwrap();

        let (_, metadata, pixels) = read_heightmap16(&output);
        assert_eq!(pixels, vec![NO_DATA16, 5]);
        assert_eq!(metadata["source"]["no_data"], -100_000);
    }

    #[test]
    fn heightmap16_vertical_datum_prefers_geotiff_keys_over_option() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("out.hmz");
        let options = HeightMap16Options {
            vertical_datum: Some("EGM96".into()),
            ..HeightMap16Options::default()
        };

        let plain = directory.path().join("plain.tif");
        GeoFixture::default().write_i16(&plain, 1, 1, &[1]);
        write_heightmap16(&plain, &output, &options).unwrap();
        assert_eq!(
            read_heightmap16(&output).1["elevation"]["vertical_datum"],
            "EGM96"
        );

        let mut keys = GEOGRAPHIC_KEYS.to_vec();
        keys.extend_from_slice(&[4096, 0, 1, 5773]);
        let coded = directory.path().join("coded.tif");
        GeoFixture {
            keys: &keys,
            ..GeoFixture::default()
        }
        .write_i16(&coded, 1, 1, &[1]);
        write_heightmap16(&coded, &output, &options).unwrap();
        assert_eq!(
            read_heightmap16(&output).1["elevation"]["vertical_datum"],
            "EPSG:5773"
        );

        let mut keys = GEOGRAPHIC_KEYS.to_vec();
        keys.extend_from_slice(&[4096, 0, 1, 32767, 4097, 34737, 12, 0]);
        let cited = directory.path().join("cited.tif");
        GeoFixture {
            keys: &keys,
            ascii: Some("EGM2008 geo|"),
            ..GeoFixture::default()
        }
        .write_i16(&cited, 1, 1, &[1]);
        write_heightmap16(&cited, &output, &options).unwrap();
        assert_eq!(
            read_heightmap16(&output).1["elevation"]["vertical_datum"],
            "EGM2008 geo"
        );
    }

    #[test]
    fn heightmap16_moves_pixel_is_point_origin_to_corner() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("point.tif");
        let output = directory.path().join("point.hmz");
        let keys = [1024, 0, 1, 2, 1025, 0, 1, 2];
        GeoFixture {
            keys: &keys,
            ..GeoFixture::default()
        }
        .write_i16(&input, 1, 1, &[1]);

        write_heightmap16(&input, &output, &HeightMap16Options::default()).unwrap();

        assert_eq!(
            read_heightmap16(&output).1["source"]["geotransform"],
            serde_json::json!([-80.25, 0.5, 0.0, 9.125, 0.0, -0.25])
        );
    }

    #[test]
    fn heightmap16_rejects_missing_or_projected_georeference() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("out.hmz");

        let plain = directory.path().join("plain.tif");
        write_fixture(&plain, 1, 1, &[1], -999);
        let message = write_heightmap16(&plain, &output, &HeightMap16Options::default())
            .unwrap_err()
            .to_string();
        assert!(message.contains("GeoKeyDirectory"), "{message}");

        let projected = directory.path().join("projected.tif");
        GeoFixture {
            keys: &[1024, 0, 1, 1],
            ..GeoFixture::default()
        }
        .write_i16(&projected, 1, 1, &[1]);
        let message = write_heightmap16(&projected, &output, &HeightMap16Options::default())
            .unwrap_err()
            .to_string();
        assert!(message.contains("model type 1"), "{message}");
        assert!(!output.exists());
    }

    #[test]
    fn heightmap16_output_is_deterministic() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("small.tif");
        let first = directory.path().join("first.hmz");
        let second = directory.path().join("second.hmz");
        GeoFixture::default().write_i16(&input, 3, 2, &[0, 10, -999, 20, 30, 40]);

        write_heightmap16(&input, &first, &HeightMap16Options::default()).unwrap();
        write_heightmap16(&input, &second, &HeightMap16Options::default()).unwrap();

        assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
    }

    #[test]
    fn converts_elevations_to_int16() {
        assert_eq!(elevation16(-32767).unwrap(), -32767);
        assert_eq!(elevation16(32767).unwrap(), 32767);
        assert!(elevation16(-32768).is_err());
        assert!(elevation16(32768).is_err());
        assert!(elevation16(i64::MIN).is_err());
    }
}
