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

/// Paquetes de cada clase que debe recorrer la barrera del formato autodelimitado, para que no
/// pierda cobertura sin que se note: código 1, código 3 de tasa constante, código 3 con relleno
/// y paquetes de más de 1275 bytes (sus tramas sí caben en una longitud autodelimitada).
#[derive(Debug, Default)]
struct Cobertura {
    codigo1: usize,
    codigo3_constante: usize,
    relleno: usize,
    mayores_de_1275: usize,
}

impl Cobertura {
    fn contar(&mut self, packet: &[u8]) {
        let codigo = packet[0] & 0x03;
        self.codigo1 += usize::from(codigo == 1);
        if codigo == 3 {
            self.codigo3_constante += usize::from(packet[1] & 0x80 == 0);
            self.relleno += usize::from(packet[1] & 0x40 != 0);
        }
        self.mayores_de_1275 += usize::from(packet.len() > 1275);
    }
}

/// Barrera del formato de libopus (RFC 6716, apéndice B; `opus_packet_parse_impl` con
/// `self_delimited`): dos flujos acoplados con los mismos paquetes de los 12 vectores, el primero
/// autodelimitado, dan lo mismo que un `OpusDecoder` estéreo en cada flujo, en todos los paquetes.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn multistream_lee_el_formato_autodelimitado_de_libopus() {
    let max = OpusDecoder::MAX_FRAME_SIZE_48K;
    let mut cobertura = Cobertura::default();
    for k in 1..=12 {
        let vector = format!("testvector{k:02}");
        let packets = load_packets(&vector).unwrap();
        let mut multi = OpusMultistreamDecoder::new(48_000, 4, 2, 2, &[0, 1, 2, 3]).unwrap();
        let mut estereo = OpusDecoder::new(48_000, 2).unwrap();
        for (n, packet) in packets.iter().enumerate() {
            cobertura.contar(packet);
            let mut est = vec![0i16; max * 2];
            let muestras = estereo.decode(packet, &mut est, false).unwrap();
            let mut pcm = vec![0i16; max * 4];
            let multi_packet = build_multistream_packet(&[packet, packet]);
            let resultado = multi.decode(&multi_packet, &mut pcm, false);
            assert!(
                matches!(resultado, Ok(m) if m == muestras),
                "{vector}, paquete {n}: {resultado:?} (se esperaba Ok({muestras}))"
            );
            for (i, trama) in pcm[..muestras * 4].chunks_exact(4).enumerate() {
                let esperado = [est[2 * i], est[2 * i + 1]];
                assert!(
                    trama[..2] == esperado && trama[2..] == esperado,
                    "{vector}, paquete {n}, muestra {i}"
                );
            }
        }
    }
    eprintln!("{cobertura:?}");
    assert!(cobertura.codigo1 > 0, "{cobertura:?}");
    assert!(cobertura.codigo3_constante > 0, "{cobertura:?}");
    assert!(cobertura.relleno > 0, "{cobertura:?}");
    assert!(cobertura.mayores_de_1275 > 0, "{cobertura:?}");
}

/// Con `fec`, si el paquete siguiente dura más que la pérdida, el multiflujo la oculta, como cada
/// flujo (`OpusDecoder::decode` con `fec`), en vez de devolver un error como libopus, que recibe
/// la duración pedida (`OPUS_BUFFER_TOO_SMALL` en `opus_multistream_decode_native`). Se prueba en
/// cada paquete de los vectores que dura más que el anterior: la llamada con `fec` da, en los dos
/// flujos, lo mismo que un `OpusDecoder` estéreo con el mismo estado.
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
#[test]
fn multistream_fec_con_paquete_mas_largo_que_la_perdida_oculta() {
    let max = OpusDecoder::MAX_FRAME_SIZE_48K;
    let mut casos = 0usize;
    for k in 1..=12 {
        let vector = format!("testvector{k:02}");
        let packets = load_packets(&vector).unwrap();
        let mut sonda = OpusDecoder::new(48_000, 2).unwrap();
        let mut sobra = vec![0i16; max * 2];
        let duraciones: Vec<usize> = packets
            .iter()
            .map(|packet| sonda.decode(packet, &mut sobra, false).unwrap())
            .collect();
        let mut multi = OpusMultistreamDecoder::new(48_000, 4, 2, 2, &[0, 1, 2, 3]).unwrap();
        let mut estereo = OpusDecoder::new(48_000, 2).unwrap();
        for (n, packet) in packets.iter().enumerate() {
            let multi_packet = build_multistream_packet(&[packet, packet]);
            let llamadas: &[bool] = if n > 0 && duraciones[n] > duraciones[n - 1] {
                casos += 1;
                &[true, false]
            } else {
                &[false]
            };
            for &fec in llamadas {
                let mut est = vec![0i16; max * 2];
                let muestras = estereo.decode(packet, &mut est, fec).unwrap();
                if fec {
                    assert_eq!(muestras, duraciones[n - 1], "{vector}, paquete {n}");
                }
                let mut pcm = vec![0i16; max * 4];
                let resultado = multi.decode(&multi_packet, &mut pcm, fec);
                assert!(
                    matches!(resultado, Ok(m) if m == muestras),
                    "{vector}, paquete {n}, fec={fec}: {resultado:?} (se esperaba Ok({muestras}))"
                );
                for (i, trama) in pcm[..muestras * 4].chunks_exact(4).enumerate() {
                    let esperado = [est[2 * i], est[2 * i + 1]];
                    assert!(
                        trama[..2] == esperado && trama[2..] == esperado,
                        "{vector}, paquete {n}, fec={fec}, muestra {i}"
                    );
                }
            }
        }
    }
    eprintln!("paquetes más largos que el anterior: {casos}");
    assert!(casos > 0, "ningún paquete dura más que el anterior");
}
