mod common;
mod compare;

use std::fs;
use std::path::{Path, PathBuf};

use common::{read_opus_demo_records, vectors_dir};
use copus_decoder::{OpusDecoder, OpusError};

/// Paquetes con `final_range` distinto que se citan en el mensaje de fallo; bastan para empezar
/// a investigar sin inundar la salida.
const MAX_REPORTED_FINAL_RANGE_MISMATCHES: usize = 8;

#[derive(Debug)]
struct Vector {
    name: String,
    bit: String,
    dec: String,
    fs_hz: u32,
    channels: u8,
}

#[test]
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
fn rfc_conformance_vectors() {
    let vectors_dir = vectors_dir();
    let manifest_path = vectors_dir.join("vectors.txt");
    let vectors = load_manifest(&manifest_path).unwrap_or_else(|e| {
        panic!(
            "failed to load manifest {}: {e}\n\n\
             Créalo con documentacion/infraestructura/preparar-entorno.ps1 o .sh\n",
            manifest_path.display(),
        )
    });

    if vectors.is_empty() {
        panic!("manifest has no vectors: {}", manifest_path.display());
    }

    let filter = std::env::var("OPUS_VECTOR").ok();
    let mut ran_any = false;

    for v in vectors {
        if let Some(f) = filter.as_deref() {
            if !v.name.contains(f) {
                continue;
            }
        }
        ran_any = true;
        run_vector(&vectors_dir, &v).unwrap_or_else(|e| {
            panic!("vector {} failed: {e}", v.name);
        });
    }

    if filter.is_some() && !ran_any {
        panic!(
            "OPUS_VECTOR filter matched nothing (manifest: {})",
            manifest_path.display()
        );
    }
}

fn run_vector(vectors_dir: &Path, v: &Vector) -> Result<(), Box<dyn std::error::Error>> {
    let bit_path = vectors_dir.join(&v.bit);
    let dec_path = vectors_dir.join(&v.dec);

    let packets = read_opus_demo_records(&bit_path)?;
    let expected_pcm = load_pcm16le(&dec_path)?;
    let alt_pcm: Option<Vec<i16>> = alt_dec_path(&dec_path)
        .filter(|p| p.is_file())
        .map(|p| load_pcm16le(&p))
        .transpose()?;

    let mut dec = OpusDecoder::new(v.fs_hz, usize::from(v.channels))?;
    let mut out = vec![0i16; dec.max_frame_size_per_channel() * usize::from(v.channels)];
    let mut got_pcm = Vec::<i16>::new();
    let mut packet_end_samples = Vec::<usize>::new();
    let mut final_range_mismatches: Vec<(usize, u32, u32)> = Vec::new();

    for (packet_idx, p) in packets.into_iter().enumerate() {
        let packet = p.packet.as_deref().unwrap_or(&[]);
        match dec.decode(packet, &mut out, false) {
            Ok(samples_per_channel) => {
                let pkt_start = got_pcm.len();
                let written = samples_per_channel * v.channels as usize;
                got_pcm.extend_from_slice(&out[..written]);
                let pkt_end = got_pcm.len();
                packet_end_samples.push(pkt_end);
                let expected_len = expected_pcm.len() / (v.channels as usize);
                if pkt_start < expected_len {
                    let compare_end = pkt_end.min(expected_len);
                    let max_delta = (pkt_start..compare_end)
                        .map(|i| (got_pcm[i] as i32 - expected_pcm[i] as i32).abs())
                        .max()
                        .unwrap_or(0);
                    if max_delta > 50 {
                        eprintln!(
                            "DELTA pkt {} max_delta={} frame_size={} sample_range={}..{}",
                            packet_idx, max_delta, written, pkt_start, pkt_end
                        );
                    }
                }
                if packet_idx == 1 && written >= 8 {
                    eprintln!(
                        "rust pcm[0..8]: {} {} {} {} {} {} {} {}",
                        out[0], out[1], out[2], out[3], out[4], out[5], out[6], out[7]
                    );
                    if expected_pcm.len() >= 968 {
                        eprintln!(
                            "ref  pcm[0..8]: {} {} {} {} {} {} {} {}",
                            expected_pcm[960],
                            expected_pcm[961],
                            expected_pcm[962],
                            expected_pcm[963],
                            expected_pcm[964],
                            expected_pcm[965],
                            expected_pcm[966],
                            expected_pcm[967]
                        );
                    }
                }
                if packet_idx < 5 {
                    eprintln!("pkt{} deemph_mem={:.4}", packet_idx, dec.deemph_mem());
                }
                let got = dec.final_range();
                if packet_idx < 5 {
                    eprintln!(
                        "pkt{} final_range expected={:08x} got={:08x}",
                        packet_idx, p.expected_final_range, got
                    );
                }
                if got != p.expected_final_range {
                    final_range_mismatches.push((packet_idx, p.expected_final_range, got));
                }
            }
            Err(OpusError::InternalError) => {
                return Err("decoder not implemented yet".into());
            }
            Err(e) => return Err(format!("decode failed: {e}").into()),
        }
    }

    // El estado final del decodificador de rango debe coincidir en todos los paquetes: un
    // desajuste indica que se leyó el flujo de bits de otra forma, aunque el PCM pase la medida
    // de calidad.
    if !final_range_mismatches.is_empty() {
        let first: Vec<String> = final_range_mismatches
            .iter()
            .take(MAX_REPORTED_FINAL_RANGE_MISMATCHES)
            .map(|(packet_idx, expected, got)| {
                format!("#{packet_idx}: expected 0x{expected:08x}, got 0x{got:08x}")
            })
            .collect();
        return Err(format!(
            "final_range mismatch in {} packets; first: {}",
            final_range_mismatches.len(),
            first.join("; ")
        )
        .into());
    }

    // Camino rápido: coincidencia exacta con alguna de las dos salidas de referencia.
    if expected_pcm == got_pcm {
        return Ok(());
    }
    if let Some(alt_pcm) = &alt_pcm {
        if *alt_pcm == got_pcm {
            return Ok(());
        }
    }

    // La conformidad de referencia usa `opus_compare` (una medida de calidad), no la
    // coincidencia exacta del PCM.
    if expected_pcm != got_pcm {
        let min_len = expected_pcm.len().min(got_pcm.len());
        let first_diff = (0..min_len).find(|&i| expected_pcm[i] != got_pcm[i]);
        if let Some(sample_idx) = first_diff {
            let packet_idx = packet_end_samples.partition_point(|&end| end <= sample_idx);
            eprintln!(
                "[RUST] first_pcm_diff sample={} packet={} expected={} got={}",
                sample_idx, packet_idx, expected_pcm[sample_idx], got_pcm[sample_idx]
            );
        }
    }
    let q0 = compare::compare_quality(
        &expected_pcm,
        &got_pcm,
        v.channels as usize,
        v.channels as usize,
        v.fs_hz,
    )?;
    if q0.passes_vectors() {
        return Ok(());
    }
    if let Some(alt_pcm) = &alt_pcm {
        if alt_pcm.len() == expected_pcm.len() {
            let q1 = compare::compare_quality(
                alt_pcm,
                &got_pcm,
                v.channels as usize,
                v.channels as usize,
                v.fs_hz,
            )?;
            if q1.passes_vectors() {
                return Ok(());
            }
            return Err(format!(
                "PCM mismatch vs both references (quality {:.1}% err {:.6} ; alt {:.1}% err {:.6})",
                q0.quality_percent,
                q0.internal_weighted_error,
                q1.quality_percent,
                q1.internal_weighted_error
            )
            .into());
        }
    }

    Err(format!(
        "PCM mismatch (quality {:.1}% err {:.6})",
        q0.quality_percent, q0.internal_weighted_error
    )
    .into())
}

fn alt_dec_path(dec_path: &Path) -> Option<PathBuf> {
    let name = dec_path.file_name()?.to_str()?;
    if !name.ends_with(".dec") || name.ends_with("m.dec") {
        return None;
    }
    let alt = name.strip_suffix(".dec")?.to_string() + "m.dec";
    Some(dec_path.with_file_name(alt))
}

fn load_manifest(path: &Path) -> Result<Vec<Vector>, Box<dyn std::error::Error>> {
    let s = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (lineno, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let name = it
            .next()
            .ok_or_else(|| format!("line {}: missing name", lineno + 1))?
            .to_string();
        let bit = it
            .next()
            .ok_or_else(|| format!("line {}: missing .bit path", lineno + 1))?
            .to_string();
        let dec = it
            .next()
            .ok_or_else(|| format!("line {}: missing .dec path", lineno + 1))?
            .to_string();
        let fs_hz = it
            .next()
            .ok_or_else(|| format!("line {}: missing fs_hz", lineno + 1))?
            .parse::<u32>()?;
        let channels = it
            .next()
            .ok_or_else(|| format!("line {}: missing channels", lineno + 1))?
            .parse::<u8>()?;
        if it.next().is_some() {
            return Err(format!("line {}: too many fields", lineno + 1).into());
        }
        out.push(Vector {
            name,
            bit,
            dec,
            fs_hz,
            channels,
        });
    }
    Ok(out)
}

fn load_pcm16le(path: &Path) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 2 != 0 {
        return Err(format!("{}: odd byte count", path.display()).into());
    }
    let mut out = Vec::<i16>::with_capacity(bytes.len() / 2);
    for c in bytes.chunks_exact(2) {
        out.push(i16::from_le_bytes([c[0], c[1]]));
    }
    Ok(out)
}
