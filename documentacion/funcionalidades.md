Funcionalidades

El decodificador de un flujo, OpusDecoder

OpusDecoder::new recibe la frecuencia de salida y el número de canales. La frecuencia puede ser
8000, 12000, 16000, 24000 o 48000 Hz, y los canales, uno o dos; cualquier otro valor devuelve
OpusError::InvalidArgument, con el nombre del parámetro equivocado.

El método decode recibe un paquete, un búfer de muestras de 16 bits y el indicador fec. Decodifica
el paquete a PCM, con las muestras entrelazadas si la salida es estéreo, y devuelve cuántas
muestras ha escrito por canal. decode_float hace lo mismo con un búfer de coma flotante; hoy esa
salida es la de 16 bits dividida entre 32 768, sin más resolución (ver los defectos conocidos y la
hoja de ruta).

El método reset devuelve el decodificador a su estado inicial, por ejemplo tras un salto en el
flujo. max_frame_size_per_channel dice el tamaño máximo de una trama por canal a la frecuencia
elegida, que es lo que debe caber en el búfer de salida. final_range devuelve el estado final del
decodificador aritmético tras el último paquete, el valor que comprueba la conformidad con RFC 8251.

Pérdidas de paquetes

Un paquete vacío activa la ocultación de paquetes perdidos: el decodificador extrapola a partir del
estado anterior. En SILK lo hace como silk_PLC_conceal de libopus: repite el tono de la última
trama buena con ruido sacado de su excitación, atenúa trama a trama, suma ruido de confort y funde
la primera trama buena con la ocultación. En CELT, como celt_decode_lost de libopus: repite el
periodo de tono de la señal anterior o, si no lo hay, rellena cada banda con ruido de su energía.
En las tramas híbridas oculta las dos capas. Una trama de un byte o de ninguno, la que manda la
transmisión discontinua (DTX), se oculta del mismo modo, con la duración que indica su cabecera.

Con fec igual a true, el paquete que se pasa es el que sigue a una pérdida, y se usa su corrección
de errores en banda (las tramas LBRR de SILK) para recuperarla, como decode_fec en libopus. La
salida dura lo que el último paquete decodificado. Si el paquete es solo CELT, si el anterior lo
era o si no trae LBRR para un canal, esa parte se oculta; en las tramas híbridas solo se recupera
la capa SILK. Después, el mismo paquete se decodifica de forma normal, con fec igual a false.

Modos y canales

Se admiten los tres modos de Opus, CELT, SILK e híbrido, las transiciones entre ellos (con la
redundancia que mandan los paquetes o, sin ella, el fundido de libopus) y todas las anchuras de
banda que define Opus. Un flujo estéreo se mezcla a mono, o uno mono se reparte a estéreo, según
los canales pedidos al crear el decodificador.

Consulta de paquetes

La función packet_has_lbrr dice si un paquete trae información de corrección de errores en banda
para el paquete anterior, como opus_packet_has_lbrr de libopus. Los paquetes solo CELT nunca la
traen, y un paquete mal formado devuelve OpusError::InvalidPacket.

El decodificador multiflujo, OpusMultistreamDecoder

OpusMultistreamDecoder::new recibe la frecuencia, los canales de salida, el número de flujos, el
número de flujos acoplados y el mapa de canales. Los primeros flujos, tantos como acoplados, son
estéreo, y el resto, mono. La entrada i del mapa dice qué canal decodificado va a la salida i, y el
valor 255 la deja en silencio. Sirve para 5.1 o 7.1, y también para juntar varios flujos mono
independientes, por ejemplo las dos partes de una llamada. Tiene decode, decode_float y reset, con
el mismo sentido que en OpusDecoder, y el indicador fec se pasa a cada flujo. El paquete
multiflujo sigue el formato de RFC 6716, anexo B, el mismo que produce libopus.

Errores

OpusError tiene cuatro variantes. InvalidPacket indica un paquete mal formado, incoherente o
demasiado grande. BufferTooSmall, que la trama decodificada no cabe en el búfer de salida.
InvalidArgument, que un parámetro de construcción no es válido; lleva un texto con el nombre del
parámetro. InternalError, un estado interno no admitido.

Lo que no hace

No codifica, no lee contenedores como Ogg, WebM o Matroska y no usa instrucciones SIMD.
