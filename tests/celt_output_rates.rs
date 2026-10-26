mod common;

use common::load_packets;
use copus_decoder::OpusDecoder;

/// Los paquetes CELT se decodifican en todas las frecuencias de salida admitidas, con las mismas
/// duraciones que a 48 kHz escaladas a cada frecuencia.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn celt_output_rates_decode_successfully() {
    let packets = load_packets("testvector07").unwrap();
    let sample_packets = &packets[..128];

    let mut reference = OpusDecoder::new(48_000, 1).unwrap();
    let mut reference_pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K];
    let mut reference_sizes = Vec::with_capacity(sample_packets.len());
    for packet in sample_packets {
        reference_sizes.push(reference.decode(packet, &mut reference_pcm, false).unwrap());
    }

    for fs in [8_000u32, 12_000, 16_000, 24_000, 48_000] {
        let mut decoder = OpusDecoder::new(fs, 1).unwrap();
        let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K];
        for (packet_idx, packet) in sample_packets.iter().enumerate() {
            let written = decoder
                .decode(packet, &mut pcm, false)
                .unwrap_or_else(|err| {
                    panic!("decode failed for fs={fs} packet={packet_idx}: {err:?}")
                });
            assert_eq!(
                written * 48_000usize,
                reference_sizes[packet_idx] * fs as usize,
                "decoded sample count should scale with the output rate"
            );
        }
    }
}

/// La ocultación de pérdidas de CELT funciona a una frecuencia de salida reducida, tras
/// decodificar unos paquetes para tener estado.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn celt_plc_supports_24k_output() {
    let packets = load_packets("testvector07").unwrap();
    let mut decoder = OpusDecoder::new(24_000, 1).unwrap();
    let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K];

    for packet in packets.iter().take(10) {
        decoder.decode(packet, &mut pcm, false).unwrap();
    }

    let written = decoder.decode(&[], &mut pcm, false).unwrap();
    assert!(
        written > 0,
        "CELT PLC should reuse the previous frame duration"
    );
    assert!(
        pcm[..written].iter().any(|&sample| sample != 0),
        "CELT PLC at 24 kHz should produce concealed output"
    );
}
