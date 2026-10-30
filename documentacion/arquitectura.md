Arquitectura

Los módulos

Todo el código está en la carpeta src. El punto de entrada, lib.rs, define la API pública,
OpusDecoder y OpusError, y delega en decoder.rs. Ese módulo es el decodificador de un flujo: elige
el modo de cada trama (CELT, SILK o híbrido), encadena las capas y se ocupa de las transiciones con
redundancia, de la ocultación de pérdidas y de la corrección de errores en banda.

A su alrededor hay piezas pequeñas, cada una con un solo asunto. multistream.rs es el decodificador
multiflujo: reparte los subpaquetes entre los flujos y aplica el mapa de canales de RFC 7845.
packet.rs lee la cabecera TOC y las tramas de un paquete, según el apartado 3 de RFC 6716.
entropy.rs es el decodificador aritmético de rango del apartado 4.1. pcm.rs fija el formato de
salida: las frecuencias admitidas y el paso a 16 bits, con los empates al par como libopus.
error.rs define el error interno, que se traduce a OpusError en la frontera pública. test_vectors.rs
da acceso a los vectores de RFC 8251 desde las pruebas unitarias.

Las dos carpetas grandes son los dos códecs de Opus. celt contiene el decodificador CELT: energía,
asignación de bits, cuantificación vectorial piramidal, bandas, anticolapso, síntesis con la
transformada MDCT inversa (synthesis.rs), postfiltro y ocultación. silk contiene el decodificador
SILK: índices, paso de NLSF a coeficientes LPC, excitación, predicción a largo plazo, estéreo,
remuestreo y ocultación, con el estado de cada canal en SilkChannel.

Por qué se parece tanto a libopus

El decodificador sigue la estructura de libopus función a función, en su variante de coma
flotante. Esa cercanía es deliberada. Permite contrastar cada función con la implementación de
referencia, y es la base de la conformidad bit a bit del estado del decodificador de rango, el
valor final_range que comprueban los vectores oficiales. También es lo que ha permitido, en la
versión 0.2.0, encontrar diferencias de una sola constante volcando las etapas internas de libopus
y de este decodificador y comparándolas trama a trama.

Las capas dependen solo hacia abajo. lib.rs usa decoder.rs, que usa CELT, SILK, packet.rs,
entropy.rs y pcm.rs; multistream.rs usa un OpusDecoder por flujo y packet.rs. Ninguna capa inferior
depende de decoder.rs: las colas de redundancia de las tramas solo SILK e híbridas se leen allí,
como en opus_decode_frame de libopus.

El camino de un paquete

Primero, OpusDecoder::decode o decode_float reciben el paquete. Si está vacío, va a la ocultación;
si fec es verdadero, decoder.rs recupera la pérdida anterior con las tramas LBRR del paquete, o la
oculta si no las trae. Después, packet.rs lee la cabecera (modo, anchura de banda, duración de
trama y estéreo) y separa las tramas; una trama de un byte o de ninguno, la de la transmisión
discontinua, se oculta como una pérdida. Por cada trama, decoder.rs elige CELT, SILK o híbrido,
gestiona las transiciones (la redundancia CELT en los cambios de modo y, sin ella, el fundido con
la ocultación del modo anterior) y mezcla los canales. CELT y SILK leen sus símbolos del mismo
decodificador de rango. Al final, la salida se pasa a 16 bits en pcm.rs, o se entrega en coma
flotante dividida entre 32 768.

Las pruebas

Las pruebas unitarias de src comprueban funciones internas como las tablas, la FFT o el
remuestreo. En la carpeta tests, cada archivo vigila un contrato distinto.

celt_output_rates.rs comprueba la salida de CELT y su ocultación a todas las frecuencias.
multistream.rs comprueba el mapa de canales, las topologías no válidas y que el multiflujo dé lo
mismo que un decodificador por flujo, con los paquetes en el formato autodelimitado de libopus.
no_panico.rs somete el decodificador a entradas aleatorias y mutadas y exige que siempre devuelva
un resultado o un error, nunca un pánico.

fuzzing.rs genera secuencias de paquetes con la estructura de RFC 6716 y mutaciones de paquetes
reales, con pérdidas y FEC, en las dos salidas, en las diez configuraciones de salida y en
multiflujo. Comprueba que no haya pánicos, que el decodificador acepte un paquete si y solo si
cumple las reglas de RFC 6716 según un oráculo escrito aparte, la duración devuelta, que nada se
escriba fuera del búfer, que el resultado sea determinista y que decode_float sea decode dividido
entre 32 768; incluye las regresiones de lo que encontró. Fuera de cargo test, la carpeta fuzz
tiene objetivos de cargo-fuzz, con su propio espacio de trabajo y Rust nightly, para los dos
decodificadores.

silk_estereo_a_mono.rs comprueba el paso de un paquete SILK estéreo a uno mono con salida estéreo,
con paquetes de libopus y a todas las frecuencias de salida. redundancia_tras_perdida.rs comprueba
que un paquete solo SILK con redundancia de CELT, tras tres pérdidas ocultadas con SILK, salga
idéntico al de libopus 1.6.1, con los datos de tests/datos.

conformance_rfc.rs pasa los doce vectores de RFC 8251 por la comparación de calidad de opus_compare
(tests/compare). diferencia_libopus.rs compara esos vectores muestra a muestra con su salida de
referencia: ni la diferencia máxima ni la suma pueden pasar de lo medido. referencia_libopus.rs
compara con libopus 1.6.1 flujos con pérdidas y FEC, pasos de estéreo a mono, dos secuencias de
ocultación, flujos multiflujo y el primero de sus flujos, a 48 kHz en estéreo y a las demás
frecuencias y canales; sus topes son lo medido, caso a caso, y necesita las referencias que genera
infraestructura/referencia-libopus/generar.sh. red_equivalencia.rs calcula 111 huellas de la
salida completa y exige que coincidan con red/huellas.txt.

tests/common/mod.rs reúne la lectura de vectores que comparten las pruebas de integración. Las que
necesitan los vectores de RFC 8251 o las referencias de libopus están marcadas para no ejecutarse
por defecto, porque dependen de datos externos; la guía de clonado explica cómo lanzarlas.

Decisiones

La licencia es la GPL 3.0 o posterior, sobre la rama Apache 2.0 del original. Es la licencia
habitual de los proyectos de Celtilander; la Apache 2.0 es compatible con la GPLv3, y la MIT solo
obliga a conservar el aviso, que se conserva.

El original, intacto, es el primer commit. Así cualquier cambio se puede comparar con la versión
publicada en crates.io, cuyo paquete tiene el SHA-256
15ae32c275dabb0cd2c863460bf349e9abc3568ba2abf0f9eb7b7d2edeeed07e, el mismo que registra el índice de
crates.io.

La red de equivalencia se construyó antes de refactorizar. Las huellas se generaron con la 0.1.1
original antes de tocar nada, y una refactorización solo se acepta si las deja idénticas; se
comprobó que la red detecta un cambio relativo de una millonésima en el filtro de desénfasis de
CELT. Una corrección que cambia la salida a propósito regenera solo las huellas que cambia, tras
comprobar que la salida se acerca a la de referencia.

La referencia para comparar es libopus 1.6.1 sin las aproximaciones rápidas de coma flotante, y
decodificada con opus_decode. Es lo que compilan por defecto Meson y CMake, y redondea a 16 bits una
sola vez, como decode; las salidas de opus_demo redondean dos veces y no sirven para comparar por
debajo de una unidad.

No se usa criterion. Era una dependencia de desarrollo solo para medir velocidad, que el paquete
publicado del original no incluía.
