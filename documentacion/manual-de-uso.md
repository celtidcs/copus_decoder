Manual de uso

Añadir la dependencia

Copus_Decoder no se publica en crates.io; se usa directamente desde su repositorio de git. En el
Cargo.toml de tu proyecto, fíjalo a una versión con su etiqueta:

    [dependencies]
    copus_decoder = { git = "https://github.com/celtidcs/copus_decoder", tag = "v0.2.0" }

Si vienes de opus-decoder, basta con cambiar la dependencia y sustituir opus_decoder por
copus_decoder en las rutas: la API es la misma.

Decodificar un flujo

Este ejemplo decodifica una lista de paquetes a 48 kHz en estéreo y junta toda la salida:

    use copus_decoder::{OpusDecoder, OpusError};

    fn decodificar_todo(paquetes: &[Vec<u8>]) -> Result<Vec<i16>, OpusError> {
        let canales = 2;
        let mut decodificador = OpusDecoder::new(48_000, canales)?;
        let mut trama = vec![0i16; decodificador.max_frame_size_per_channel() * canales];
        let mut salida = Vec::new();
        for paquete in paquetes {
            let muestras = decodificador.decode(paquete, &mut trama, false)?;
            salida.extend_from_slice(&trama[..muestras * canales]);
        }
        Ok(salida)
    }

El búfer de salida debe caber una trama completa, es decir, max_frame_size_per_channel por el
número de canales; si es menor, la llamada devuelve OpusError::BufferTooSmall. El valor devuelto
cuenta muestras por canal: en estéreo, el búfer recibe el doble, entrelazado como izquierdo,
derecho, izquierdo, derecho. Para obtener la salida en coma flotante se usa decode_float con un
búfer de f32; ten en cuenta que, en esta versión, esa salida tiene la misma resolución que la de 16
bits.

Paquetes perdidos

Cuando falta un paquete, por una pérdida en la red o un hueco en el archivo, llama a decode con un
paquete vacío. El decodificador genera una trama de ocultación de la misma duración que la última
decodificada; si todavía no ha decodificado ninguna, devuelve cero muestras.

    let muestras = decodificador.decode(&[], &mut trama, false)?;

Si el flujo trae corrección de errores en banda, puedes recuperar la pérdida en vez de ocultarla.
Cuando llegue el paquete siguiente, decodifícalo primero con fec igual a true, lo que reconstruye
la trama perdida a partir de los datos de respaldo que ese paquete lleva, y después otra vez con
fec igual a false, lo que da su propia trama. La función packet_has_lbrr dice de antemano si el
paquete trae con qué recuperar.

    let recuperadas = decodificador.decode(&siguiente, &mut trama, true)?;
    let propias = decodificador.decode(&siguiente, &mut trama, false)?;

Varios flujos a la vez

Para juntar dos flujos mono independientes en una salida estéreo, con el flujo 0 a la izquierda y
el 1 a la derecha:

    use copus_decoder::OpusMultistreamDecoder;

    let mut decodificador = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1])?;
    let mut pcm = vec![0f32; copus_decoder::OpusDecoder::MAX_FRAME_SIZE_48K * 2];
    let muestras = decodificador.decode_float(&paquete_multiflujo, &mut pcm, false)?;

Y para un 5.1 con dos flujos estéreo y dos mono, dejando en silencio el último canal:

    let decodificador = OpusMultistreamDecoder::new(48_000, 6, 4, 2, &[0, 1, 2, 3, 4, 255])?;

El paquete multiflujo sigue el formato de RFC 6716, anexo B, el mismo que produce
opus_multistream_encode de libopus: cada subpaquete salvo el último lleva su propia delimitación,
con la longitud de su última trama tras la cabecera, y el último va en el formato normal.

Saltos dentro del flujo

Tras un salto, por ejemplo una búsqueda en un archivo, llama a reset antes de decodificar el
paquete siguiente, para que el estado del punto anterior no se mezcle con el nuevo.
