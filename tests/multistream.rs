mod common;

use common::{build_multistream_packet, load_packets};
use copus_decoder::{OpusDecoder, OpusMultistreamDecoder};

/// El caso de la grabación de llamadas: dos flujos mono independientes, uno por canal.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn multistream_stereo_call_recording() {
    let packets = load_packets("testvector07").unwrap();
    let packet = build_multistream_packet(&[&packets[0], &packets[1]]);

    let mut decoder = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1]).unwrap();
    let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K * 2];
    let samples = decoder.decode(&packet, &mut pcm, false).unwrap();

    assert!(samples > 0);
    assert!(
        pcm[..samples * 2]
            .chunks_exact(2)
            .any(|frame| frame[0] != frame[1])
    );
}

/// Un `255` en el mapa deja en silencio su canal de salida.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn multistream_silence_channel() {
    let packets = load_packets("testvector07").unwrap();
    let packet = &packets[0];

    let mut decoder = OpusMultistreamDecoder::new(48_000, 2, 1, 0, &[0, 255]).unwrap();
    let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K * 2];
    let samples = decoder.decode(packet, &mut pcm, false).unwrap();

    assert!(samples > 0);
    assert!(
        pcm[..samples * 2]
            .chunks_exact(2)
            .all(|frame| frame[1] == 0)
    );
}

/// Se rechazan las topologías de multiflujo no válidas.
#[test]
fn multistream_invalid_args() {
    assert!(OpusMultistreamDecoder::new(48_000, 2, 1, 2, &[0, 1]).is_err());
    assert!(OpusMultistreamDecoder::new(48_000, 2, 1, 0, &[0]).is_err());
    assert!(OpusMultistreamDecoder::new(48_000, 2, 1, 0, &[0, 2]).is_err());
}

/// Mapa de la barrera: el canal mono, los dos del flujo acoplado separados por uno en silencio.
const MAPA_BARRERA: [u8; 4] = [2, 0, 255, 1];
/// Paquetes de `testvector01` que recorre la barrera, con estado entre uno y otro. Son todos
/// estéreo: con paquetes mono (los primeros de `testvector07`), el decodificador estéreo saca dos
/// canales iguales y la barrera no vería un intercambio de izquierdo y derecho (medido).
const PAQUETES_BARRERA: usize = 50;

/// Reparte, como debe hacerlo el multiflujo con `MAPA_BARRERA`, la salida de un decodificador
/// estéreo y otra de uno mono en cuatro canales entrelazados.
fn repartir<T: Copy + Default>(estereo: &[T], mono: &[T], muestras: usize) -> Vec<T> {
    let canales = MAPA_BARRERA.len();
    let mut salida = vec![T::default(); muestras * canales];
    for (i, trama) in salida.chunks_exact_mut(canales).enumerate() {
        trama[0] = mono[i];
        trama[1] = estereo[2 * i];
        trama[3] = estereo[2 * i + 1];
    }
    salida
}

/// Barrera del multiflujo con oráculo independiente: con un flujo acoplado y otro mono, su
/// salida `i16` y `f32` es, muestra a muestra, la de un `OpusDecoder` estéreo y otro mono con
/// los mismos subpaquetes, repartida según el mapa y con el canal 255 a cero; también al
/// ocultar una pérdida.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn multistream_matches_stream_decoders() {
    let packets = load_packets("testvector01").unwrap();
    let canales = MAPA_BARRERA.len();
    let mut multi_i16 = OpusMultistreamDecoder::new(48_000, canales, 2, 1, &MAPA_BARRERA).unwrap();
    let mut multi_f32 = OpusMultistreamDecoder::new(48_000, canales, 2, 1, &MAPA_BARRERA).unwrap();
    let mut estereo_i16 = OpusDecoder::new(48_000, 2).unwrap();
    let mut mono_i16 = OpusDecoder::new(48_000, 1).unwrap();
    let mut estereo_f32 = OpusDecoder::new(48_000, 2).unwrap();
    let mut mono_f32 = OpusDecoder::new(48_000, 1).unwrap();
    let max = OpusDecoder::MAX_FRAME_SIZE_48K;

    for (n, packet) in packets.iter().take(PAQUETES_BARRERA).enumerate() {
        let multi_packet = build_multistream_packet(&[packet, packet]);

        let mut pcm = vec![0i16; max * canales];
        let muestras = multi_i16.decode(&multi_packet, &mut pcm, false).unwrap();
        let mut est = vec![0i16; max * 2];
        let mut mon = vec![0i16; max];
        assert_eq!(
            estereo_i16.decode(packet, &mut est, false).unwrap(),
            muestras
        );
        assert_eq!(mono_i16.decode(packet, &mut mon, false).unwrap(), muestras);
        assert_eq!(
            pcm[..muestras * canales],
            repartir(&est, &mon, muestras)[..],
            "i16, paquete {n}"
        );

        let mut pcm = vec![0.0f32; max * canales];
        let muestras = multi_f32
            .decode_float(&multi_packet, &mut pcm, false)
            .unwrap();
        let mut est = vec![0.0f32; max * 2];
        let mut mon = vec![0.0f32; max];
        assert_eq!(
            estereo_f32.decode_float(packet, &mut est, false).unwrap(),
            muestras
        );
        assert_eq!(
            mono_f32.decode_float(packet, &mut mon, false).unwrap(),
            muestras
        );
        assert_eq!(
            pcm[..muestras * canales],
            repartir(&est, &mon, muestras)[..],
            "f32, paquete {n}"
        );
    }

    // Una pérdida (paquete vacío) se oculta con la duración del último paquete, en el
    // multiflujo igual que en cada flujo.
    let mut pcm = vec![0i16; max * canales];
    let muestras = multi_i16.decode(&[], &mut pcm, false).unwrap();
    let mut est = vec![0i16; max * 2];
    let mut mon = vec![0i16; max];
    assert_eq!(estereo_i16.decode(&[], &mut est, false).unwrap(), muestras);
    assert_eq!(mono_i16.decode(&[], &mut mon, false).unwrap(), muestras);
    assert!(muestras > 0);
    assert_eq!(
        pcm[..muestras * canales],
        repartir(&est, &mon, muestras)[..],
        "i16, ocultación"
    );
}
