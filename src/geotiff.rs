//! Reads the georeference and vertical datum of a geographic GeoTIFF.

use std::io::{Read, Seek};

use tiff::decoder::Decoder;
use tiff::tags::Tag;

use crate::{Result, error};

const MODEL_PIXEL_SCALE: Tag = Tag::Unknown(33550);
const MODEL_TIEPOINT: Tag = Tag::Unknown(33922);
const MODEL_TRANSFORMATION: Tag = Tag::Unknown(34264);
const GEO_KEY_DIRECTORY: Tag = Tag::Unknown(34735);
const GEO_ASCII_PARAMS: Tag = Tag::Unknown(34737);

const GT_MODEL_TYPE: u16 = 1024;
const GT_RASTER_TYPE: u16 = 1025;
const GEOG_ANGULAR_UNITS: u16 = 2054;
const VERTICAL_CS_TYPE: u16 = 4096;
const VERTICAL_CITATION: u16 = 4097;
const VERTICAL_DATUM: u16 = 4098;

const MODEL_TYPE_GEOGRAPHIC: u16 = 2;
const RASTER_PIXEL_IS_POINT: u16 = 2;
const ANGULAR_DEGREE: u16 = 9102;
const USER_DEFINED: u16 = 32767;

const WGS84_SEMI_MAJOR_AXIS: f64 = 6_378_137.0;
const WGS84_FLATTENING: f64 = 1.0 / 298.257_223_563;

/// The source raster's position on the Earth.
#[derive(Clone, Debug, PartialEq)]
pub struct Georeference {
    /// GDAL-style affine transform from the top-left corner of source pixel
    /// `(column, row)` to longitude and latitude in degrees:
    /// `longitude = g[0] + column * g[1] + row * g[2]` and
    /// `latitude = g[3] + column * g[4] + row * g[5]`.
    pub geotransform: [f64; 6],
    /// The vertical datum declared by the GeoTIFF keys, if any.
    pub vertical_datum: Option<String>,
}

pub fn read_georeference<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<Georeference> {
    let keys = GeoKeys::read(decoder)?;

    match keys.short(GT_MODEL_TYPE) {
        Some(MODEL_TYPE_GEOGRAPHIC) => {}
        Some(model_type) => {
            return Err(error(format!(
                "unsupported GeoTIFF model type {model_type}: expected geographic longitude and latitude"
            )));
        }
        None => return Err(error("GeoTIFF does not declare a model type")),
    }
    if let Some(units) = keys.short(GEOG_ANGULAR_UNITS)
        && units != ANGULAR_DEGREE
    {
        return Err(error(format!(
            "unsupported GeoTIFF angular units {units}: expected degrees"
        )));
    }

    let mut geotransform = read_geotransform(decoder)?;
    if keys.short(GT_RASTER_TYPE) == Some(RASTER_PIXEL_IS_POINT) {
        // The tiepoint names the center of the first pixel; move it to the corner.
        geotransform[0] -= geotransform[1] / 2.0;
        geotransform[3] -= geotransform[5] / 2.0;
    }

    let vertical_datum = match keys.short(VERTICAL_CS_TYPE) {
        Some(code) if code != 0 && code != USER_DEFINED => Some(format!("EPSG:{code}")),
        _ => match keys.ascii(VERTICAL_CITATION) {
            Some(citation) => Some(citation),
            None => match keys.short(VERTICAL_DATUM) {
                Some(code) if code != 0 && code != USER_DEFINED => Some(format!("EPSG:{code}")),
                _ => None,
            },
        },
    };

    Ok(Georeference {
        geotransform,
        vertical_datum,
    })
}

fn read_geotransform<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<[f64; 6]> {
    let geotransform = if let Some(value) = decoder.find_tag(MODEL_TRANSFORMATION)? {
        let matrix = value.into_f64_vec()?;
        if matrix.len() != 16 {
            return Err(error(format!(
                "GeoTIFF ModelTransformation has {} values, expected 16",
                matrix.len()
            )));
        }
        [
            matrix[3], matrix[0], matrix[1], matrix[7], matrix[4], matrix[5],
        ]
    } else {
        let tiepoint = match decoder.find_tag(MODEL_TIEPOINT)? {
            Some(value) => value.into_f64_vec()?,
            None => return Err(error("GeoTIFF has no ModelTiepoint or ModelTransformation")),
        };
        if tiepoint.len() != 6 {
            return Err(error(format!(
                "unsupported GeoTIFF ModelTiepoint with {} values: expected exactly one tiepoint",
                tiepoint.len()
            )));
        }
        let scale = match decoder.find_tag(MODEL_PIXEL_SCALE)? {
            Some(value) => value.into_f64_vec()?,
            None => return Err(error("GeoTIFF has a ModelTiepoint but no ModelPixelScale")),
        };
        if scale.len() < 2 || !(scale[0] > 0.0 && scale[1] > 0.0) {
            return Err(error(format!(
                "invalid GeoTIFF ModelPixelScale {scale:?}: expected positive x and y scales"
            )));
        }
        [
            tiepoint[3] - tiepoint[0] * scale[0],
            scale[0],
            0.0,
            tiepoint[4] + tiepoint[1] * scale[1],
            0.0,
            -scale[1],
        ]
    };

    if geotransform.iter().any(|value| !value.is_finite()) {
        return Err(error(format!(
            "GeoTIFF geotransform {geotransform:?} is not finite"
        )));
    }
    if geotransform[2] != 0.0 || geotransform[4] != 0.0 {
        return Err(error("unsupported rotated or sheared GeoTIFF geotransform"));
    }
    if geotransform[1] == 0.0 || geotransform[5] == 0.0 {
        return Err(error("GeoTIFF geotransform has a zero pixel size"));
    }
    Ok(geotransform)
}

/// Returns the north–south size in meters of one source pixel at the raster's
/// center latitude on the WGS 84 ellipsoid.
pub fn nominal_pixel_size_m(geotransform: &[f64; 6], height: u32) -> Result<f64> {
    let latitude = geotransform[3] + geotransform[5] * f64::from(height) / 2.0;
    if !(-90.0..=90.0).contains(&latitude) {
        return Err(error(format!(
            "raster center latitude {latitude} is outside -90..=90 degrees"
        )));
    }
    let eccentricity_squared = WGS84_FLATTENING * (2.0 - WGS84_FLATTENING);
    let sine = latitude.to_radians().sin();
    let meridional_radius = WGS84_SEMI_MAJOR_AXIS * (1.0 - eccentricity_squared)
        / (1.0 - eccentricity_squared * sine * sine).powf(1.5);
    Ok(geotransform[5].abs().to_radians() * meridional_radius)
}

struct GeoKeys {
    directory: Vec<u16>,
    ascii: Option<String>,
}

impl GeoKeys {
    fn read<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<Self> {
        let Some(directory) = decoder.find_tag_unsigned_vec::<u16>(GEO_KEY_DIRECTORY)? else {
            return Err(error("input is not a GeoTIFF: missing GeoKeyDirectory"));
        };
        let key_count = usize::from(*directory.get(3).unwrap_or(&0));
        if directory.len() < 4 + key_count * 4 {
            return Err(error("GeoTIFF GeoKeyDirectory is truncated"));
        }
        let ascii = match decoder.find_tag(GEO_ASCII_PARAMS)? {
            Some(value) => Some(value.into_string()?),
            None => None,
        };
        Ok(Self { directory, ascii })
    }

    fn entry(&self, key: u16) -> Option<[u16; 4]> {
        let key_count = usize::from(self.directory[3]);
        self.directory[4..4 + key_count * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .find(|entry| entry[0] == key)
            .copied()
    }

    fn short(&self, key: u16) -> Option<u16> {
        let [_, location, count, value] = self.entry(key)?;
        (location == 0 && count == 1).then_some(value)
    }

    fn ascii(&self, key: u16) -> Option<String> {
        let [_, location, count, offset] = self.entry(key)?;
        if location != GEO_ASCII_PARAMS.to_u16() {
            return None;
        }
        let start = usize::from(offset);
        let end = start.checked_add(usize::from(count))?;
        let text = self.ascii.as_deref()?.get(start..end)?;
        let text = text.trim_end_matches(['|', '\0']).trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_panama_nominal_pixel_size() {
        let geotransform = [
            -83.059_722_222_222_22,
            1.0 / 3600.0,
            0.0,
            9.650_555_555_555_552,
            0.0,
            -1.0 / 3600.0,
        ];
        let size = nominal_pixel_size_m(&geotransform, 8868).unwrap();
        assert!((size - 30.72).abs() < 0.01, "{size}");
    }

    #[test]
    fn rejects_center_latitude_off_the_globe() {
        let geotransform = [0.0, 1.0, 0.0, 100.0, 0.0, -1.0];
        assert!(nominal_pixel_size_m(&geotransform, 2).is_err());
    }
}
