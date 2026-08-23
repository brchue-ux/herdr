//! Clipboard image byte formats: DIB (Windows device-independent bitmap) to
//! PNG, and PNG validation.
//!
//! The one caller is the Windows clipboard reader
//! ([`super::windows::read_clipboard_image`]) — Windows is the only platform
//! whose clipboard hands Herdr raw bitmap bytes rather than a PNG a helper
//! process already produced (`pngpaste` on macOS, `wl-paste`/`xclip` on Linux).
//!
//! It nevertheless lives here rather than beside that reader, as a
//! `cfg(any(windows, test))` module of the shared platform layer, because
//! nothing in it is an OS API: it is a pure byte-format decoder over a slice,
//! exactly the "testable contract" the platform boundary is meant to hold. The
//! practical consequence is the point — the parser and its tests compile and
//! **run** on a Unix dev box and in CI, where the Win32 calls that feed it can
//! only be type-checked. This is the half of the Windows clipboard bridge that
//! can be verified without a Windows machine, so it is the half that is
//! deliberately not welded to one.
//!
//! Every input here is untrusted: a DIB on the clipboard was put there by
//! whatever application the user last copied from. Nothing indexes without a
//! bounds check, no length arithmetic is unchecked, dimensions are capped
//! before any allocation is sized from them, and the encoded result is written
//! through a writer that refuses to exceed the protocol's own payload limit.

use std::io::{self, Cursor, Write as _};

use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;

/// Ceiling on a single `GlobalAlloc` block Herdr will copy off the clipboard.
///
/// Larger than [`MAX_CLIPBOARD_IMAGE_PAYLOAD`] because a DIB is uncompressed:
/// a bitmap that encodes to a PNG well under the protocol limit can easily
/// occupy several times that as raw BGRA rows. The dimension caps below are
/// what actually bound the decode; this only bounds the copy.
pub(super) const MAX_CLIPBOARD_ALLOCATION: usize = 64 * 1024 * 1024;

/// Equal limits would make some legitimate screenshots unreadable before the
/// decoder ever saw them, so the relationship is a compile-time invariant
/// rather than a comment.
const _: () = assert!(MAX_CLIPBOARD_ALLOCATION > MAX_CLIPBOARD_IMAGE_PAYLOAD);

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: usize = 16 * 1024 * 1024;
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Accepts `bytes` as a PNG only if it fully decodes, trimming the allocator
/// tail the registered `"PNG"` clipboard format routinely carries.
///
/// `GlobalAlloc` rounds a clipboard block up, so the handle for a registered
/// PNG is usually a few bytes longer than the PNG inside it and `GlobalSize`
/// reports the rounded length. The logical length is read from the stream's own
/// chunk table instead, then the trimmed bytes are decoded end to end — a PNG
/// that only *starts* well is not forwarded.
pub(super) fn validated_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let logical_len = png_logical_len(bytes)?;
    if logical_len > MAX_CLIPBOARD_IMAGE_PAYLOAD {
        return None;
    }
    let bytes = bytes.get(..logical_len)?;
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_CLIPBOARD_IMAGE_PAYLOAD,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().ok()?;
    validate_dimensions(reader.info().width, reader.info().height)?;
    if reader.info().animation_control.is_some() {
        return None;
    }
    while reader.next_interlaced_row().ok()?.is_some() {}
    reader.finish().ok()?;
    Some(bytes.to_vec())
}

/// Walks the chunk table to the end of `IEND`, which is where the PNG ends and
/// any allocator padding begins.
fn png_logical_len(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut offset = PNG_SIGNATURE.len();
    while offset < bytes.len() {
        let length = usize::try_from(read_u32_be(bytes, offset)?).ok()?;
        let kind_start = offset.checked_add(4)?;
        let data_start = kind_start.checked_add(4)?;
        let data_end = data_start.checked_add(length)?;
        let chunk_end = data_end.checked_add(4)?;
        if chunk_end > bytes.len() {
            return None;
        }
        if bytes.get(kind_start..data_start)? == b"IEND" {
            return (length == 0).then_some(chunk_end);
        }
        offset = chunk_end;
    }
    None
}

/// Converts a packed DIB — the bytes behind `CF_DIBV5`/`CF_DIB` — to a PNG,
/// or `None` if it is malformed or in a layout Herdr does not decode.
pub(super) fn dib_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    Dib::parse(bytes)?.encode_png()
}

struct Dib<'a> {
    width: u32,
    height: u32,
    top_down: bool,
    stride: usize,
    pixels: &'a [u8],
    format: PixelFormat,
}

#[derive(Clone, Copy)]
enum PixelFormat {
    Bgr24,
    Bgrx32,
    Masked32 {
        red: ChannelMask,
        green: ChannelMask,
        blue: ChannelMask,
        alpha: Option<ChannelMask>,
    },
}

#[derive(Clone, Copy)]
struct ChannelMask {
    mask: u32,
    shift: u32,
    maximum: u32,
}

impl ChannelMask {
    /// A channel mask is one contiguous run of set bits; anything else is
    /// rejected rather than approximated.
    fn parse(mask: u32) -> Option<Self> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        let maximum = mask >> shift;
        if maximum & maximum.wrapping_add(1) != 0 {
            return None;
        }
        Some(Self {
            mask,
            shift,
            maximum,
        })
    }

    fn extract(self, pixel: u32) -> u8 {
        let value = (pixel & self.mask) >> self.shift;
        ((u64::from(value) * 255 + u64::from(self.maximum) / 2) / u64::from(self.maximum)) as u8
    }
}

impl<'a> Dib<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let header_size = usize::try_from(read_u32_le(bytes, 0)?).ok()?;
        if !matches!(header_size, 40 | 52 | 56 | 108 | 124) || bytes.len() < header_size {
            return None;
        }
        let signed_width = read_i32_le(bytes, 4)?;
        let signed_height = read_i32_le(bytes, 8)?;
        if signed_width <= 0 || signed_height == 0 || signed_height == i32::MIN {
            return None;
        }
        let width = u32::try_from(signed_width).ok()?;
        let height = signed_height.unsigned_abs();
        validate_dimensions(width, height)?;
        if read_u16_le(bytes, 12)? != 1 {
            return None;
        }

        let bit_count = read_u16_le(bytes, 14)?;
        let compression = read_u32_le(bytes, 16)?;
        let colors_used = usize::try_from(read_u32_le(bytes, 32)?).ok()?;
        let (format, external_mask_bytes) =
            pixel_format(bytes, header_size, bit_count, compression)?;
        let palette_bytes = colors_used.checked_mul(4)?;
        let pixel_offset = header_size
            .checked_add(external_mask_bytes)?
            .checked_add(palette_bytes)?;
        let row_bits = usize::try_from(width)
            .ok()?
            .checked_mul(usize::from(bit_count))?;
        let stride = row_bits.checked_add(31)?.checked_div(32)?.checked_mul(4)?;
        let image_size = stride.checked_mul(usize::try_from(height).ok()?)?;
        let pixels = bytes.get(pixel_offset..pixel_offset.checked_add(image_size)?)?;

        Some(Self {
            width,
            height,
            // A negative height means the rows are stored top row first; the
            // Windows default is bottom row first.
            top_down: signed_height < 0,
            stride,
            pixels,
            format,
        })
    }

    fn encode_png(&self) -> Option<Vec<u8>> {
        let mut output = LimitedWriter::new(MAX_CLIPBOARD_IMAGE_PAYLOAD);
        {
            let mut encoder = png::Encoder::new(&mut output, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            let mut stream = writer.stream_writer().ok()?;
            let mut rgba = vec![0_u8; usize::try_from(self.width).ok()?.checked_mul(4)?];
            for output_row in 0..usize::try_from(self.height).ok()? {
                let source_row = if self.top_down {
                    output_row
                } else {
                    usize::try_from(self.height)
                        .ok()?
                        .checked_sub(output_row.checked_add(1)?)?
                };
                let start = source_row.checked_mul(self.stride)?;
                let source = self.pixels.get(start..start.checked_add(self.stride)?)?;
                self.decode_row(source, &mut rgba)?;
                stream.write_all(&rgba).ok()?;
            }
            stream.finish().ok()?;
            writer.finish().ok()?;
        }
        Some(output.into_inner())
    }

    fn decode_row(&self, source: &[u8], output: &mut [u8]) -> Option<()> {
        let width = usize::try_from(self.width).ok()?;
        match self.format {
            PixelFormat::Bgr24 => {
                for (pixel, rgba) in source
                    .chunks_exact(3)
                    .take(width)
                    .zip(output.chunks_exact_mut(4))
                {
                    rgba.copy_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            PixelFormat::Bgrx32 => {
                for (pixel, rgba) in source
                    .chunks_exact(4)
                    .take(width)
                    .zip(output.chunks_exact_mut(4))
                {
                    rgba.copy_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            PixelFormat::Masked32 {
                red,
                green,
                blue,
                alpha,
            } => {
                for (pixel, rgba) in source
                    .chunks_exact(4)
                    .take(width)
                    .zip(output.chunks_exact_mut(4))
                {
                    let pixel = u32::from_le_bytes(pixel.try_into().ok()?);
                    rgba.copy_from_slice(&[
                        red.extract(pixel),
                        green.extract(pixel),
                        blue.extract(pixel),
                        alpha.map_or(255, |mask| mask.extract(pixel)),
                    ]);
                }
            }
        }
        Some(())
    }
}

/// Resolves the pixel layout, and how many bytes of channel masks sit between
/// the header and the pixels.
///
/// `BITMAPINFOHEADER` (40 bytes) carries `BI_BITFIELDS` masks *after* the
/// header, so they displace the pixel data; every later header revision has
/// the mask fields inside itself and displaces nothing.
fn pixel_format(
    bytes: &[u8],
    header_size: usize,
    bit_count: u16,
    compression: u32,
) -> Option<(PixelFormat, usize)> {
    match (bit_count, compression) {
        (24, BI_RGB) => return Some((PixelFormat::Bgr24, 0)),
        (32, BI_RGB) => return Some((PixelFormat::Bgrx32, 0)),
        (32, BI_BITFIELDS | BI_ALPHABITFIELDS) => {}
        _ => return None,
    }

    let (offset, external_mask_bytes) = if header_size == 40 {
        let count = if compression == BI_ALPHABITFIELDS {
            4
        } else {
            3
        };
        (40, count * 4)
    } else {
        if header_size < 52 || (compression == BI_ALPHABITFIELDS && header_size < 56) {
            return None;
        }
        (40, 0)
    };
    let red = ChannelMask::parse(read_u32_le(bytes, offset)?)?;
    let green = ChannelMask::parse(read_u32_le(bytes, offset + 4)?)?;
    let blue = ChannelMask::parse(read_u32_le(bytes, offset + 8)?)?;
    if red.mask & green.mask != 0 || red.mask & blue.mask != 0 || green.mask & blue.mask != 0 {
        return None;
    }
    let alpha = if compression == BI_ALPHABITFIELDS || header_size >= 56 {
        match read_u32_le(bytes, offset + 12).filter(|mask| *mask != 0) {
            Some(mask) => Some(ChannelMask::parse(mask)?),
            None => None,
        }
    } else {
        None
    };
    if alpha.is_some_and(|alpha| alpha.mask & (red.mask | green.mask | blue.mask) != 0) {
        return None;
    }
    Some((
        PixelFormat::Masked32 {
            red,
            green,
            blue,
            alpha,
        },
        external_mask_bytes,
    ))
}

fn validate_dimensions(width: u32, height: u32) -> Option<()> {
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return None;
    }
    let pixels = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    (pixels <= MAX_IMAGE_PIXELS).then_some(())
}

/// A `Write` sink that fails rather than growing past `limit`.
///
/// The encoder writes incrementally, so this refuses the first write that would
/// cross the protocol's payload ceiling instead of letting a pathological
/// bitmap allocate its way there first.
struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded clipboard image exceeds protocol limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BGRA_RED_MASK: u32 = 0x00FF_0000;
    const BGRA_GREEN_MASK: u32 = 0x0000_FF00;
    const BGRA_BLUE_MASK: u32 = 0x0000_00FF;
    const BGRA_ALPHA_MASK: u32 = 0xFF00_0000;

    fn info_header(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
        sized_header(40, width, height, bit_count, compression)
    }

    fn sized_header(
        header_size: u32,
        width: i32,
        height: i32,
        bit_count: u16,
        compression: u32,
    ) -> Vec<u8> {
        let mut header = vec![0_u8; header_size as usize];
        header[0..4].copy_from_slice(&header_size.to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header[12..14].copy_from_slice(&1_u16.to_le_bytes());
        header[14..16].copy_from_slice(&bit_count.to_le_bytes());
        header[16..20].copy_from_slice(&compression.to_le_bytes());
        header
    }

    fn put_masks(header: &mut [u8], offset: usize, masks: &[u32]) {
        for (index, mask) in masks.iter().enumerate() {
            let start = offset + index * 4;
            header[start..start + 4].copy_from_slice(&mask.to_le_bytes());
        }
    }

    fn decode_rgba(bytes: &[u8]) -> Vec<u8> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0_u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut output).unwrap();
        output.truncate(info.buffer_size());
        output
    }

    fn encode_rgba_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
        bytes
    }

    #[test]
    fn converts_bottom_up_24_bit_dib_to_png() {
        let mut dib = info_header(2, 2, 24, BI_RGB);
        dib.extend_from_slice(&[
            255, 0, 0, 255, 255, 255, 0, 0, // bottom: blue, white, padding
            0, 0, 255, 0, 255, 0, 0, 0, // top: red, green, padding
        ]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(
            decode_rgba(&png),
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn strips_registered_png_allocator_tail() {
        let expected = encode_rgba_png(1, 1, &[1, 2, 3, 255]);
        let mut bytes = expected.clone();
        bytes.extend_from_slice(&[0; 32]);

        assert_eq!(validated_png(&bytes), Some(expected));
    }

    #[test]
    fn converts_top_down_24_bit_dib_without_flipping_rows() {
        let mut dib = info_header(2, -2, 24, BI_RGB);
        dib.extend_from_slice(&[
            0, 0, 255, 0, 255, 0, 0, 0, // top: red, green, padding
            255, 0, 0, 255, 255, 255, 0, 0, // bottom: blue, white, padding
        ]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(
            decode_rgba(&png),
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn honors_24_bit_row_padding_at_a_width_that_is_not_a_multiple_of_four() {
        // Three 24-bit pixels are nine bytes; the row is padded to twelve.
        let mut dib = info_header(3, 1, 24, BI_RGB);
        dib.extend_from_slice(&[
            255, 0, 0, // blue
            0, 255, 0, // green
            0, 0, 255, // red
            9, 9, 9, // padding, must never be read as a pixel
        ]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(
            decode_rgba(&png),
            vec![0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn converts_32_bit_bi_rgb_dib_as_opaque() {
        let mut dib = info_header(1, 1, 32, BI_RGB);
        // BGRX: the fourth byte is undefined padding, not alpha.
        dib.extend_from_slice(&[10, 20, 30, 0]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(decode_rgba(&png), vec![30, 20, 10, 255]);
    }

    #[test]
    fn reads_bitfield_masks_stored_after_a_40_byte_header() {
        let mut dib = info_header(2, 1, 32, BI_BITFIELDS);
        dib.extend_from_slice(&BGRA_RED_MASK.to_le_bytes());
        dib.extend_from_slice(&BGRA_GREEN_MASK.to_le_bytes());
        dib.extend_from_slice(&BGRA_BLUE_MASK.to_le_bytes());
        dib.extend_from_slice(&0x0000_00FF_u32.to_le_bytes()); // blue pixel
        dib.extend_from_slice(&0x0000_FF00_u32.to_le_bytes()); // green pixel

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(decode_rgba(&png), vec![0, 0, 255, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn reads_alpha_bitfield_masks_stored_after_a_40_byte_header() {
        let mut dib = info_header(1, 1, 32, BI_ALPHABITFIELDS);
        dib.extend_from_slice(&BGRA_RED_MASK.to_le_bytes());
        dib.extend_from_slice(&BGRA_GREEN_MASK.to_le_bytes());
        dib.extend_from_slice(&BGRA_BLUE_MASK.to_le_bytes());
        dib.extend_from_slice(&BGRA_ALPHA_MASK.to_le_bytes());
        dib.extend_from_slice(&0x8000_00FF_u32.to_le_bytes());

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(decode_rgba(&png), vec![0, 0, 255, 128]);
    }

    #[test]
    fn reads_bitfield_masks_stored_inside_a_v5_header() {
        // CF_DIBV5 is the format Windows itself puts on the clipboard for a
        // screenshot, so the in-header mask layout is the common case.
        let mut dib = sized_header(124, 1, 1, 32, BI_BITFIELDS);
        put_masks(
            &mut dib,
            40,
            &[
                BGRA_RED_MASK,
                BGRA_GREEN_MASK,
                BGRA_BLUE_MASK,
                BGRA_ALPHA_MASK,
            ],
        );
        dib.extend_from_slice(&0xFF00_FF00_u32.to_le_bytes());

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(decode_rgba(&png), vec![0, 255, 0, 255]);
    }

    #[test]
    fn skips_a_palette_declared_by_clr_used_before_the_pixels() {
        let mut dib = info_header(1, 1, 24, BI_RGB);
        dib[32..36].copy_from_slice(&2_u32.to_le_bytes());
        dib.extend_from_slice(&[7; 8]); // two palette entries, never pixels
        dib.extend_from_slice(&[255, 0, 0, 0]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(decode_rgba(&png), vec![0, 0, 255, 255]);
    }

    #[test]
    fn rejects_malformed_and_unsupported_dibs() {
        // Truncated pixel data.
        let mut short = info_header(2, 2, 24, BI_RGB);
        short.extend_from_slice(&[0; 8]);
        assert_eq!(dib_to_png(&short), None);

        // Header size no revision of the DIB header ever had.
        let mut odd_header = sized_header(41, 1, 1, 24, BI_RGB);
        odd_header.extend_from_slice(&[0; 4]);
        assert_eq!(dib_to_png(&odd_header), None);

        // Header shorter than it claims.
        assert_eq!(dib_to_png(&sized_header(40, 1, 1, 24, BI_RGB)[..20]), None);

        // Dimensions past the caps, and degenerate dimensions.
        assert_eq!(dib_to_png(&info_header(20_000, 1, 24, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(1, 20_000, 24, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(0, 1, 24, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(1, 0, 24, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(-1, 1, 24, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(1, i32::MIN, 24, BI_RGB)), None);
        // 16384 x 16384 is inside both dimension caps and past the pixel cap.
        assert_eq!(dib_to_png(&info_header(16_384, 16_384, 24, BI_RGB)), None);

        // Bit depths and compressions Herdr does not decode.
        assert_eq!(dib_to_png(&info_header(1, 1, 8, BI_RGB)), None);
        assert_eq!(dib_to_png(&info_header(1, 1, 16, BI_BITFIELDS)), None);
        assert_eq!(dib_to_png(&info_header(1, 1, 32, 1 /* BI_RLE8 */)), None);

        // Plane count is always one.
        let mut two_planes = info_header(1, 1, 24, BI_RGB);
        two_planes[12..14].copy_from_slice(&2_u16.to_le_bytes());
        two_planes.extend_from_slice(&[0; 4]);
        assert_eq!(dib_to_png(&two_planes), None);

        // Empty and stub inputs.
        assert_eq!(dib_to_png(&[]), None);
        assert_eq!(dib_to_png(&[40, 0, 0]), None);
    }

    #[test]
    fn rejects_channel_masks_that_are_not_disjoint_contiguous_runs() {
        let masked = |red: u32, green: u32, blue: u32, alpha: u32| {
            let mut dib = info_header(1, 1, 32, BI_ALPHABITFIELDS);
            dib.extend_from_slice(&red.to_le_bytes());
            dib.extend_from_slice(&green.to_le_bytes());
            dib.extend_from_slice(&blue.to_le_bytes());
            dib.extend_from_slice(&alpha.to_le_bytes());
            dib.extend_from_slice(&[0; 4]);
            dib
        };

        // Zero mask.
        assert_eq!(
            dib_to_png(&masked(0, BGRA_GREEN_MASK, BGRA_BLUE_MASK, BGRA_ALPHA_MASK)),
            None
        );
        // Non-contiguous run.
        assert_eq!(
            dib_to_png(&masked(
                0x00F0_F000,
                BGRA_GREEN_MASK,
                BGRA_BLUE_MASK,
                BGRA_ALPHA_MASK
            )),
            None
        );
        // Colour channels overlapping each other.
        assert_eq!(
            dib_to_png(&masked(
                BGRA_GREEN_MASK,
                BGRA_GREEN_MASK,
                BGRA_BLUE_MASK,
                BGRA_ALPHA_MASK
            )),
            None
        );
        // Alpha overlapping a colour channel.
        assert_eq!(
            dib_to_png(&masked(
                BGRA_RED_MASK,
                BGRA_GREEN_MASK,
                BGRA_BLUE_MASK,
                BGRA_BLUE_MASK
            )),
            None
        );
        // A 52-byte header has no alpha mask field, so BI_ALPHABITFIELDS in one
        // is a lie about its own length.
        let mut v2_alpha = sized_header(52, 1, 1, 32, BI_ALPHABITFIELDS);
        put_masks(
            &mut v2_alpha,
            40,
            &[BGRA_RED_MASK, BGRA_GREEN_MASK, BGRA_BLUE_MASK],
        );
        v2_alpha.extend_from_slice(&[0; 4]);
        assert_eq!(dib_to_png(&v2_alpha), None);
    }

    #[test]
    fn validated_png_rejects_bytes_that_are_not_a_complete_png() {
        let png = encode_rgba_png(1, 1, &[1, 2, 3, 255]);

        assert_eq!(validated_png(b"not a png at all"), None);
        assert_eq!(validated_png(&[]), None);
        // Signature only, no chunk table.
        assert_eq!(validated_png(PNG_SIGNATURE), None);
        // Truncated before IEND.
        assert_eq!(validated_png(&png[..png.len() - 8]), None);
        // A chunk whose declared length runs past the buffer.
        let mut lying_length = png.clone();
        lying_length[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(validated_png(&lying_length), None);
        // Intact table, corrupt image data.
        let mut corrupt = png.clone();
        let idat = corrupt.len() - 20;
        corrupt[idat] ^= 0xFF;
        assert_eq!(validated_png(&corrupt), None);
    }

    #[test]
    fn validated_png_rejects_dimensions_past_the_cap() {
        let too_wide = encode_rgba_png(
            MAX_IMAGE_DIMENSION + 1,
            1,
            &vec![0_u8; (MAX_IMAGE_DIMENSION as usize + 1) * 4],
        );

        assert_eq!(validated_png(&too_wide), None);
    }

    #[test]
    fn validated_png_rejects_an_animated_png() {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(2, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 255]).unwrap();
        writer.write_image_data(&[3, 2, 1, 255]).unwrap();
        writer.finish().unwrap();

        assert_eq!(validated_png(&bytes), None);
    }

    #[test]
    fn limited_writer_refuses_the_write_that_would_cross_its_limit() {
        use std::io::Write as _;

        let mut writer = LimitedWriter::new(4);
        assert!(writer.write_all(b"abc").is_ok());
        assert!(writer.write_all(b"de").is_err());
        assert!(writer.write_all(b"d").is_ok());
        assert_eq!(writer.into_inner(), b"abcd");
    }
}
