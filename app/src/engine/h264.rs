//! The two framings of H.264 NAL units: Annex B (start codes), which the
//! decoder and encoder speak, and length prefixes, which MP4 stores.

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;

/// NAL units of an Annex B byte stream, without their start codes.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                nals.push(trim_zeros(&data[s..i]));
            }
            start = Some(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        nals.push(trim_zeros(&data[s..]));
    }
    nals.retain(|n| !n.is_empty());
    nals
}

/// Zero bytes in front of the next start code belong to that start code.
fn trim_zeros(mut nal: &[u8]) -> &[u8] {
    while let [rest @ .., 0] = nal {
        nal = rest;
    }
    nal
}

pub fn push_annex_b(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

/// Appends an MP4 sample (length-prefixed NAL units) to `out` as Annex B.
pub fn sample_to_annex_b(
    sample: &[u8],
    length_size: usize,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let mut pos = 0;
    while pos < sample.len() {
        if pos + length_size > sample.len() {
            return Err("truncated NAL unit length".into());
        }
        let len = sample[pos..pos + length_size]
            .iter()
            .fold(0usize, |n, b| (n << 8) | *b as usize);
        pos += length_size;
        if len > sample.len() - pos {
            return Err("NAL unit runs past the end of its sample".into());
        }
        push_annex_b(out, &sample[pos..pos + len]);
        pos += len;
    }
    Ok(())
}

/// Appends the picture data of an Annex B access unit to `out` with 4-byte
/// length prefixes. Parameter sets, delimiters and SEI are left out: they
/// live in the init segment, and an `avc1` track must not repeat them.
pub fn access_unit_to_sample(access_unit: &[u8], out: &mut Vec<u8>) {
    for nal in split_annex_b(access_unit) {
        if matches!(nal[0] & 0x1f, NAL_SLICE..=NAL_IDR) {
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_round_trip() {
        let stream = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0,
        ];
        let nals = split_annex_b(&stream);
        assert_eq!(
            nals,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]
        );

        let mut sample = Vec::new();
        access_unit_to_sample(&stream, &mut sample);
        assert_eq!(sample, [0, 0, 0, 3, 0x65, 9, 9]);

        let mut back = Vec::new();
        sample_to_annex_b(&sample, 4, &mut back).unwrap();
        assert_eq!(back, [0, 0, 0, 1, 0x65, 9, 9]);
        assert!(sample_to_annex_b(&[0, 0, 0, 9, 1], 4, &mut back).is_err());
    }
}
