Historial de cambios

Cambios de Copus_Decoder respecto a opus-decoder 0.1.1. El historial de git conserva cada paso por
separado, empezando por el paquete original intacto.

Versión 0.2.0, del 30 de octubre de 2026

La 0.2.0 acerca el decodificador a libopus 1.6.1, la implementación de referencia de Opus, en todo
lo que el original dejaba a medias: la ocultación de paquetes perdidos de SILK y de CELT, la
corrección de errores en banda (FEC), el multiflujo en el formato que produce libopus, todas las
frecuencias de salida y las transiciones entre modos. Cada cambio se ha medido contra salidas de
libopus generadas con un guion público y reproducible, y el decodificador ha pasado más de diez
millones de pasos de fuzzing sin un solo pánico. Sin pérdidas, a 48 kHz en estéreo, la salida de
los doce vectores oficiales difiere de su referencia como mucho en una unidad de 16 bits (cuatro en
el vector 08), algo que no se oye. Lo que todavía no coincide está en los defectos conocidos, y el
plan para la siguiente versión, en la hoja de ruta.

Correcciones de la versión 0.2.0

El modo anterior es el de la trama anterior, no el del paquete anterior. Como opus_decode_frame de
libopus (src/opus_decoder.c), el modo y la redundancia anteriores se actualizan tras cada trama.
Antes se actualizaban tras el paquete, y en la segunda trama de un paquete híbrido de dos tramas que
seguía a uno solo CELT se volvía a reiniciar CELT. Medido con los vectores de RFC 8251 a 48 kHz en
estéreo frente a la salida de referencia: solo cambia el 10, cuya suma de diferencias absolutas baja
de 40 075 a 17 509 y su diferencia máxima de 33 a 2. La red de equivalencia se regenera solo en las
9 huellas de ese vector.

Fundido de CELT al pasar de híbrido a solo SILK. Como opus_decode_frame de libopus, la primera trama
solo SILK tras una híbrida suma una trama CELT de silencio de 2,5 ms, con la que se extingue la
superposición de la MDCT de CELT; antes no se hacía. Medido igual: solo cambia el vector 12, el
único con esa transición, cuya suma de diferencias absolutas baja de 950 a 654 y su diferencia
máxima de 10 a 1.

El multiflujo lee el formato de libopus. OpusMultistreamDecoder esperaba delante de cada subpaquete,
salvo el último, la longitud del subpaquete entero, un formato que no produce ningún codificador.
Ahora lee la delimitación propia de RFC 6716, anexo B, como opus_multistream_decode de libopus: la
longitud de la última trama va tras el TOC, el byte de cuenta, el relleno y las demás longitudes.
Antes no se decodificaba ningún paquete multiflujo de libopus; ahora, con flujos codificados con
libopus 1.6.1, la diferencia con su salida es la de un solo flujo (máximo 1) salvo en las
transiciones de modo y en la ocultación que ya figuran en los defectos conocidos. Se rechaza, como
en libopus, el paquete más corto que 2 × flujos − 1 bytes o sin bytes para el último flujo.

Cambios de frecuencia interna y de mono a estéreo en SILK como en libopus. Al cambiar la frecuencia
interna, el decodificador reiniciaba todo el estado de síntesis del canal; ahora, como
silk_decoder_set_fs de libopus, solo la salida reciente, la memoria LPC, el lag, el último índice de
ganancia y el tipo de señal, y conserva la ganancia anterior y la excitación. Al pasar de un paquete
mono a uno estéreo, el canal derecho conservaba lo que le quedó del último tramo estéreo; ahora
empieza de cero, como con silk_init_decoder en silk_Decode, con cualquier número de canales de
salida. Medido frente a libopus 1.6.1 a 48 kHz en estéreo con un flujo de paquetes de los vectores
02, 03 y 04 intercalados de 7 en 7 (142 cambios de frecuencia): sin pérdidas, 5 paquetes difieren
(hasta 441 en i16) antes del cambio y ninguno después; con una pérdida de cada 9 paquetes o con
ráfagas de 3, la diferencia máxima baja de 2 424 a 0. Con los vectores de RFC 8251, el 10, que pasa
de mono a estéreo en tramas híbridas, baja de una diferencia máxima de 2 a 1 con la referencia, y el
12 con una pérdida de cada 9 paquetes, de 752 a 281 frente a libopus 1.6.1. La red de equivalencia
cambia en las 9 huellas del vector 10 y en la del 12 con pérdidas.

Con salida mono, SILK saca el canal medio, como libopus. De un paquete SILK o híbrido estéreo, el
decodificador mono convertía medio y lateral a izquierdo y derecho y sacaba el izquierdo.
silk_Decode de libopus solo hace esa conversión si el paquete y la salida son estéreo; si no, saca
el medio, retrasado una muestra como el de un paquete mono. Medido frente a libopus 1.6.1 a 48 kHz
en mono: los vectores 02, 03 y 04 de RFC 8251 y los flujos con DTX y con cambios de frecuencia
interna, solos, con una pérdida de cada 9 paquetes y con ráfagas de 3, pasan de diferir hasta 2 630
en i16 a ser idénticos. La red de equivalencia cambia en las 24 huellas de salida mono de los
vectores con paquetes SILK o híbridos estéreo (02, 03, 04, 05, 06, 08, 09 y 10, a 48, 16 y 8 kHz) y
en la del multiflujo tv05+tv12, cuyos flujos son mono.

SILK remuestrea a todas las frecuencias de salida, como libopus. El remuestreador solo convertía a
48 kHz; a 24, 16, 12 y 8 kHz el decodificador copiaba el PCM interno sin convertirlo. Ahora es un
port de silk/resampler.c de libopus 1.6.1 para el decodificador: según la pareja de frecuencias,
copia, sube al doble con los filtros paso todo, sube con ellos y un FIR fraccionario, o baja con un
filtro AR de segundo orden y un FIR (3/4, 2/3 y 1/2), cada pareja con su retardo de entrada. Medido
frente a libopus 1.6.1 a 24 kHz en estéreo, 16 kHz en mono, 12 kHz en estéreo y 8 kHz en mono: los
vectores 02, 03 y 04 de RFC 8251 y los flujos con DTX y con cambios de frecuencia interna, solos y
con pérdidas, pasan de diferir hasta 11 756 en i16 a ser idénticos. La salida a 48 kHz no cambia. La
red de equivalencia cambia en las 36 huellas a 24, 16, 12 y 8 kHz de los vectores con paquetes SILK
o híbridos (02, 03, 04, 05, 06, 08, 09, 10 y 12).

CELT se recorta al Nyquist de la salida antes de diezmar, como libopus. Fuera de 48 kHz, CELT
sintetiza a 48 kHz y toma una de cada 2, 3, 4 o 6 muestras sin filtrar; libopus anula antes el
espectro por encima del Nyquist de la salida (denormalise_bands, en la decodificación y en la
ocultación), y aquí no se hacía, así que esa parte se replegaba. Medido frente a libopus 1.6.1 con
el vector 12 de RFC 8251 a 16 kHz en mono, 12 kHz en estéreo y 8 kHz en mono, solo y con pérdidas:
los paquetes híbridos pasan de diferir hasta 453 en i16 a una diferencia máxima de 1, como a 48 kHz.
La salida a 48 kHz no cambia. La red de equivalencia cambia en 34 huellas: las de 24, 16, 12 y 8 kHz
de los vectores con paquetes de banda completa (01, 06, 07, 08, 09, 10 y 11) y las de 16, 12 y 8 kHz
de los que solo tienen híbridos de banda superancha (05 y 12).

Ocultación de pérdidas de CELT y del modo híbrido como libopus. La pérdida se oculta en trozos, como
opus_decode_frame de libopus: de 20 ms como mucho y, por debajo, de 10 o de 5 ms, así que una
pérdida de 12,5 ms se oculta como 10 + 2,5; antes CELT devolvía silencio en las duraciones que no
son suyas. Se oculta con el modo de la última trama, o con CELT si esta traía redundancia CELT. En
híbrido, a la ocultación de SILK se le suma el ruido de CELT desde la banda 17; antes solo se
ocultaba SILK. El ruido de CELT es el de celt_decode_lost (energía que cae 1,5 la primera vez y 0,5
después sin bajar de la de fondo, la semilla con signo, la banda final de la última trama y el
postfiltro con sus últimos parámetros), y la primera trama buena tras la pérdida hace segura su
predicción de energía y deja subir la energía de fondo lo que habría subido durante la pérdida.
Medido frente a libopus 1.6.1 a 48 kHz en estéreo con los vectores 05, 06 y 12 de RFC 8251 con
pérdidas: las tramas ocultadas y las buenas pasan de diferir hasta 1 707 en i16 a como mucho 1, la
diferencia que tienen sin pérdidas. La ocultación periódica de CELT sigue pendiente. La red de
equivalencia cambia en 9 de sus 12 casos con pérdidas: los de los vectores con paquetes CELT o
híbridos.

Ocultación periódica de CELT como libopus. Las pérdidas de paquetes solo CELT se ocultan repitiendo
el periodo de tono de la señal anterior, como celt_decode_lost de libopus 1.6.1
(FRAME_PLC_PERIODIC): búsqueda del periodo, predicción lineal de orden 24, extrapolación con caída
por periodo, filtro de síntesis y atenuación si la energía crece; la segunda pérdida seguida
reutiliza periodo y predicción, y la trama siguiente funde el solapamiento (prefilter_and_fold). Se
oculta con ruido, como libopus, si la pérdida dura ya 100 ms o no han llegado dos tramas buenas
seguidas. Los productos y correlaciones se calculan en el orden de los núcleos x86 de libopus con
AVX2 (src/celt/x86_kernels.rs), sin SIMD. Además, cada trozo de ocultación dura como mucho una trama
del último paquete, como en libopus; antes podía durar 20 ms tras tramas más cortas. Medido frente a
libopus 1.6.1 a 48 kHz en estéreo con los vectores 01, 07, 08, 09, 10 y 11 con pérdidas: de diferir
hasta 29 173 en i16 a como mucho 39, salvo dos casos del 10 (defectos conocidos). La red de
equivalencia cambia en los 6 casos con pérdidas de esos vectores.

La síntesis de CELT redondea como libopus. La MDCT inversa usaba una transformada discreta de
Fourier calculada por la definición en f64 (coste cuadrático), con los giros calculados con cos;
ahora es un port de opus_fft_impl (FFT de base mixta 2, 3, 4 y 5 en f32) y de clt_mdct_backward de
libopus 1.6.1, con las tablas de giros y de inversión de dígitos de static_modes_float.h copiadas
literalmente (src/celt/static_tables.rs): libopus usa esas tablas, impresas con 8 cifras, y no las
que se calcularían. Además, como libopus en x86-64: la parte de ganancia constante del postfiltro
suma como comb_filter_const_sse; renormalise_vector y stereo_merge calculan sus productos escalares
como celt_inner_prod_sse; y la ganancia de renormalise_vector y de los pulsos es el inverso de la
raíz por la ganancia. Medido frente a libopus 1.6.1 a 48 kHz en estéreo, la suma de diferencias
absolutas sin pérdidas baja en todos los vectores con CELT (el 11, de 107 524 a 809; el 01, de 41
658 a 6 282; el 09, de 5 958 a 692; el 08, de 10 627 a 5 517); la diferencia máxima no cambia (1, y
4 en el 08). Con pérdidas, la del 10 con una pérdida de cada 9 paquetes baja de 3 760 a 3, y la del
07 con ráfagas, de 39 a 1; en cambio, el 08 con una pérdida de cada 9 sube de 9 a 23, porque la
búsqueda de tono de la ocultación elige otro periodo, y en el 12 con pérdidas la suma de las
ocultadas sube en 1 o 2 (defectos conocidos). La transformada pasa de coste cuadrático a O(n log n):
la red de equivalencia tarda 6 s en vez de 4 minutos. La red cambia en 79 huellas: las de los
vectores con paquetes CELT o híbridos, salvo las de 05 y 06 a 16, 12 y 8 kHz y las del 12 a 12 y 8
kHz, donde la parte CELT queda por encima del Nyquist de la salida; las de paquetes dañados de los
vectores solo SILK (02, 03 y 04), cuyos paquetes dañados traen redundancia CELT; y las de multiflujo
y transiciones.

Fundido de los cambios de modo sin redundancia como libopus. Cuando un paquete cambia de modo sin la
trama redundante de la transición (al entrar en CELT desde SILK o híbrido sin redundancia previa, o
al salir de CELT si la trama no la trae, por ejemplo porque se perdió el paquete que la llevaba),
opus_decode_frame oculta 5 ms (2,5 en las tramas CELT de 2,5 ms) con el estado del modo anterior y
funde con ellos el principio de la trama nueva: con 5 ms o más, copia 2,5 ms y funde los 2,5
siguientes; con 2,5 ms, funde la trama entera sin copiar. Antes, al salir de CELT no se fundía nada
y al entrar en CELT se fundía con silencio, copiando 1,25 ms en las tramas de 2,5 ms. Medido frente
a libopus 1.6.1 a 48 kHz en estéreo: el vector 10 con ráfagas de pérdidas baja de 4 161 a 2 (el
paquete 1400, híbrido tras perder los que traían la redundancia desde CELT); el multiflujo varios,
de 19 370 a 310 (suma de 39 966 691 a 24 926), con las transiciones de modo en 14 como mucho; lo
demás no cambia. La red de equivalencia cambia en 3 huellas: los paquetes dañados de los vectores 08
y 10 y el caso de transiciones.

Una trama de más de 1275 bytes hace inválido el paquete. No se comprobaba la longitud implícita de
la última trama (de todas, en el código 1 y en el 3 de tasa constante), y se aceptaban tramas de
hasta 1499 bytes. RFC 6716 lo prohíbe (regla R2 de §3.4) y libopus las rechaza
(opus_packet_parse_impl). Ahora devuelven InvalidPacket, también como último subpaquete de un
multiflujo. Lo encontró el fuzzing.

Sin tope de 1500 bytes por paquete. Los paquetes de más de 1500 bytes se rechazaban, aunque fueran
válidos (por ejemplo, dos tramas de 1275 bytes, 48 tramas de 2,5 ms o un relleno largo), igual que
los multiflujos que pasaban de ese tamaño. Ni RFC 6716 ni libopus limitan la longitud de un paquete:
solo la de sus tramas (1275 bytes) y su duración (120 ms). El coste sigue acotado por la duración, y
el relleno no se lee. Lo encontró el fuzzing.

El código 1 sin carga es válido. Un paquete de código 1 de un solo byte (el TOC) son dos tramas de 0
bytes; RFC 6716 solo exige que la carga tenga una longitud par, y libopus lo acepta. Se rechazaba,
también como subpaquete de un multiflujo; ahora sus dos tramas se ocultan como las de la DTX. Lo
encontró el fuzzing.

decode_float con FEC valida el paquete antes que la salida. Con fec = true, un paquete inválido y
una salida corta, decode_float devolvía BufferTooSmall y decode, InvalidPacket, como libopus, que
analiza el paquete antes de mirar el tamaño de la salida. Ahora los dos devuelven InvalidPacket. Lo
encontró el fuzzing.

Paso de estéreo a mono en SILK como en libopus. Con salida estéreo, cuando un paquete SILK o híbrido
mono sigue a uno estéreo de la misma frecuencia interna, silk_Decode de libopus (stereo_to_mono,
silk/dec_API.c) saca el canal derecho de la primera trama interna por el remuestreador del derecho,
que conserva el estado del tramo estéreo, y desde la segunda copia el izquierdo. El decodificador
copiaba siempre el izquierdo, y el derecho saltaba en los primeros milisegundos del paquete. Medido
frente a libopus 1.6.1 con el multiflujo varios, cuyo primer flujo pasa 28 veces de estéreo a mono
en tramas solo SILK: la diferencia máxima de ese flujo baja de 310 a 1, y la del caso entero, de 310
a 17 (suma de 24 926 a 16 546). El vector 10 de RFC 8251 pasa 10 veces de estéreo a mono en tramas
híbridas: sin pérdidas y con una pérdida de cada 9 paquetes no cambia, y con ráfagas de 3 la suma de
diferencias baja de 33 502 a 33 487. Con salida mono no cambia nada. La red de equivalencia cambia
en 2 huellas del vector 10: la salida a 12 kHz en estéreo y los paquetes dañados a 48 kHz.

La redundancia CELT→SILK no se mezcla tras ocultar en SILK. Como opus_decode_frame de libopus, la
trama CELT redundante de un paquete solo SILK que sigue a uno solo CELT se decodifica siempre, pero
solo se mezcla en los primeros 5 ms de la salida si la trama anterior no era solo SILK o traía
redundancia SILK→CELT. En las tramas híbridas ya era así; en las solo SILK se mezclaba siempre, y
tras perder el paquete CELT y ocultarlo con SILK la salida difería de libopus. En el primer flujo de
varios, con ráfagas de 3 pérdidas, la diferencia máxima baja de 2 316 a 2, y con una pérdida de cada
9, de 1 564 a 82. Sin pérdidas no cambia nada. La red de equivalencia cambia solo en las 4 huellas
de paquetes dañados de los vectores SILK (02, 03, 04 y 12): los 124 paquetes que cambian son SILK
dañados tras otro SILK, en los que el daño hace leer esa redundancia.

Rotación de dispersión de CELT y redondeo a 16 bits como libopus. exp_rotation calcula sus dos
coeficientes como celt_cos_norm de libopus en coma flotante (celt/vq.c, celt/mathops.h): el coseno
en double de (π/2)·θ y de (π/2)·(1 − θ), pasado a float; antes, cos y sin en f32 de (π/2)·θ, que
diferían en el último bit. La salida i16 redondea los empates al par, como FLOAT2INT16
(celt/float_cast.h); antes, lejos del cero. Frente a libopus 1.6.1 sin aproximaciones, a 48 kHz en
estéreo: el vector 11 baja de 610 muestras distintas a 4; el 01, de 6 245 a 5 925; el 11 con una
pérdida de cada 9, de una diferencia máxima de 173 a 2, y el 07 igual, de 74 a 26; las tramas
ocultadas de la secuencia plc_celt, de 86 a ninguna. A 48 kHz en mono, el 11 con una pérdida de cada
9 baja de 2 178 a 1. Suben dos casos de ocultación con el filtro LPC mal condicionado (defectos
conocidos): el primer flujo de varios con una pérdida de cada 9, en estéreo a 16, 24 y 48 kHz, de
(24, 1) a (82, 94) (máximo en las tramas buenas y en las ocultadas), y el 01 con ráfagas de 3, a 12
kHz en mono, de (1, 3) a (91, 296). La red de equivalencia cambia en 72 de sus 111 huellas: las de
flujos con CELT, las i16 con empates y las de paquetes dañados; no cambia ninguna de los vectores
solo SILK (02, 03 y 04) sin daños ni del 12.

Ganancia de los ángulos estéreo extremos de CELT como libopus. Con el ángulo estéreo de una banda a
0 o a un cuarto de vuelta, la ganancia del canal medio o del lateral es 32 767/32 768, como en
compute_theta de libopus (celt/bands.c); antes era 1. En las bandas de dos coeficientes, que solo
existen en tramas de 2,5 y 5 ms, el espectro normalizado difería por ese factor; en las demás, en el
último bit. Ahora el espectro normalizado de los vectores 01 y 11 es idéntico al de libopus 1.6.1
sin aproximaciones en todas las tramas. Frente a libopus, a 48 kHz en estéreo: el 01 baja de 5 925
muestras distintas a 9; el 10, de 9 746 a 773; el 11, de 4 a 2; el 07 con una pérdida de cada 9, de
una diferencia máxima de 26 a 2 en las ocultadas; el primer flujo de varios con pérdidas, de
(82, 94) a (24, 1). De 337 comparaciones, bajan 134 y sube una: el 07 con una pérdida de cada 9 a 16 kHz
en estéreo, de 53 a 156 en las ocultadas (su suma baja), una ocultación fuera de 48 kHz (defectos
conocidos). Frente a las salidas de RFC 8251, el 01 baja de 6 184 a 346. La red de equivalencia
cambia en 62 de 111 huellas, todas de flujos con CELT o híbridos en estéreo o de paquetes dañados.

Funcionalidades nuevas de la versión 0.2.0

Ocultación de pérdidas de SILK como la de libopus. La ocultación heredada repetía la última trama
con una atenuación lineal. Ahora es un port de silk/PLC.c y silk/CNG.c de libopus 1.6.1: el tono y
la excitación de la última trama buena, con los LPC expandidos y la historia reblanqueada; la
atenuación por subtrama; el ruido de confort; el fundido de la primera trama buena con la
ocultación; la expansión de los LPC y el suavizado del tono en esa trama; y el canal lateral y la
predicción estéreo como en silk_Decode. Medido frente a libopus 1.6.1 a 48 kHz en estéreo: los
vectores solo SILK de RFC 8251 (02, 03 y 04) con una pérdida de cada 9 paquetes o con ráfagas de 3
son idénticos muestra a muestra (antes, hasta 38 872 en i16), y también el flujo con FEC, tanto en
las tramas recuperadas como en las ocultadas.

DTX. Una trama de un byte o ninguno se oculta como una pérdida, como en opus_decode_frame de
libopus; antes se intentaba decodificar. Un flujo SILK con DTX codificado con libopus 1.6.1, solo y
con pérdidas, sale idéntico al de libopus.

Corrección de errores en banda (FEC). decode y decode_float con fec = true recuperan la pérdida
anterior con las tramas LBRR del paquete siguiente, como decode_fec de libopus: la duración es la
del último paquete, se oculta lo que no se pueda recuperar y, en las tramas híbridas, solo se
recupera la capa SILK. Medido frente a libopus 1.6.1 con un flujo de voz con FEC y una pérdida de
cada 9 paquetes: la suma de diferencias de las tramas recuperadas baja de 3 398 126 677 a 921 813
898, y la de las tramas buenas, de 20 459 352 541 a 4 959 915 427; las recuperadas a partir del
mismo estado coinciden con libopus muestra a muestra salvo cuando hay que ocultar el canal lateral,
porque la ocultación de SILK no es la de libopus.

FEC en banda en el multiflujo. OpusMultistreamDecoder::decode y decode_float con fec = true parten
el paquete siguiente y pasan fec a cada flujo, como opus_multistream_decode de libopus; antes
ocultaban la pérdida en todos los flujos. Cada flujo da lo mismo que un OpusDecoder con fec. Si el
paquete siguiente dura más que la pérdida, se oculta, como en un solo flujo (libopus devolvería un
error). Medido frente a libopus 1.6.1 con un flujo multiflujo con FEC y una pérdida de cada 9
paquetes: la suma de diferencias de las tramas perdidas baja de 6 220 094 381 a 2 773 426 448, y la
de las buenas, de 26 264 501 288 a 3 389 066 073.

packet_has_lbrr. Función pública que dice si un paquete trae información de corrección de errores en
banda (LBRR) para el anterior, como opus_packet_has_lbrr de libopus. Sobre los flujos de prueba con
FEC cuenta exactamente los mismos paquetes que libopus 1.6.1.

Pruebas de la versión 0.2.0

Fuzzing con oráculo. tests/fuzzing.rs decodifica secuencias de paquetes generados con la estructura
de RFC 6716, cargados hacia los límites de cada regla, y mutaciones de paquetes reales, con pérdidas
y FEC, en i16 y f32, en las 10 configuraciones de salida y en cuatro multiflujos. Comprueba en cada
llamada que no hay pánicos; que un paquete se rechaza si y solo si incumple alguna regla R1–R7 de
§3.4 (o, en multiflujo, las del apéndice B), según un oráculo escrito aparte; que la duración es la
correcta; que la salida demasiado corta se rechaza y nada se escribe tras lo decodificado; que dos
decodificadores dan lo mismo bit a bit; que decode_float es decode / 32768; y que tras un error se
sigue decodificando. El oráculo da por válidos los 73 202 paquetes de los vectores y de las
referencias de libopus, y su duración total es la de la salida de libopus en los 20 flujos sin
pérdidas. Encontró los cuatro defectos corregidos arriba (tramas de más de 1275 bytes, el tope de
1500, el código 1 sin carga y el orden de los errores de decode_float con FEC), y ningún pánico. Con
ellos corregidos, la campaña larga pasa sin fallos 10 millones de pasos en release y un millón en
depuración. Una ronda corta corre con cargo test; la campaña larga, con --ignored.

Fuzzing guiado por cobertura. fuzz/ (espacio de trabajo propio, con cargo-fuzz y Rust nightly) tiene
dos objetivos, uno de OpusDecoder y otro de OpusMultistreamDecoder, que decodifican secuencias de
paquetes, pérdidas y FEC con decode y decode_float a la vez y comprueban que no hay pánicos, que los
dos dan lo mismo y que nada pasa de la salida. Con semillas de paquetes reales, una hora por
objetivo en Linux no dio ningún fallo. Sobre los 14 764 paquetes distintos del corpus resultante,
libopus 1.6.1 y este decodificador aceptan y rechazan exactamente los mismos, con la misma duración.

Paso de estéreo a mono. tests/silk_estereo_a_mono.rs decodifica con salida estéreo un paquete SILK
estéreo de libopus seguido de uno mono: a la misma frecuencia interna, el derecho de la primera
trama interna no puede ser una copia del izquierdo (a 48 kHz) y el de la segunda sí; con otra
frecuencia interna, lo copia entero; a todas las frecuencias de salida. Corre con cargo test, sin
referencias externas.

Multiflujo frente a libopus. tests/referencia_libopus.rs decodifica seis flujos multiflujo
codificados con libopus 1.6.1 (3 canales; tasas, duraciones, relleno y pérdidas distintas) y falla
si la diferencia pasa de la medida; tests/multistream.rs comprueba con todos los paquetes de los 12
vectores, en formato autodelimitado, que cada flujo da lo mismo que un OpusDecoder.

FEC frente a libopus. tests/referencia_libopus.rs decodifica los flujos con FEC como opus_demo de
libopus 1.6.1 y falla si la diferencia de las tramas buenas, ocultadas o recuperadas pasa de la
medida. Necesita las salidas de referencia de libopus.

Ocultación de SILK frente a libopus. tests/referencia_libopus.rs exige que los vectores 02, 03 y 04
con pérdidas y el flujo con DTX salgan idénticos a libopus.

Cambios de frecuencia interna frente a libopus. tests/referencia_libopus.rs exige que el flujo con
cambios de frecuencia interna y de mono a estéreo, solo y con pérdidas, salga idéntico a libopus;
dos pruebas unitarias de src/silk/mod.rs comprueban, además, que el canal derecho se reinicia
también con salida mono y que el cambio de frecuencia conserva la ganancia anterior y la excitación.

Salida mono frente a libopus. tests/referencia_libopus.rs exige que los flujos solo SILK (vectores
02, 03 y 04, DTX y cambios de frecuencia interna, solos y con pérdidas) decodificados a 48 kHz en
mono salgan idénticos a libopus, y que el vector 12, híbrido, no se aleje más de lo medido.

Frecuencias de salida frente a libopus. tests/referencia_libopus.rs exige que esos mismos flujos
solo SILK salgan idénticos a libopus a 24, 16, 12 y 8 kHz, y que el vector 12 no se aleje más de lo
medido.

Recorte de CELT al Nyquist de la salida. tests/referencia_libopus.rs exige que el vector 12 a 16, 12
y 8 kHz, solo y con pérdidas, no difiera de libopus más de 1; dos pruebas unitarias de src/celt/
comprueban que el espectro queda a cero desde el Nyquist de la salida y que, en la ocultación a 16
kHz, la energía de las bandas por encima de 8 kHz no cambia la salida.

Ocultación híbrida frente a libopus. tests/referencia_libopus.rs exige que los vectores 05, 06 y 12
con pérdidas no difieran de libopus más que sin pérdidas (1), y baja a esa cifra los topes del
vector 12 a 48 kHz en mono y a 24 kHz; pruebas unitarias de src/decoder.rs y src/celt/plc.rs
comprueban los trozos de la pérdida, la caída de energía y la predicción segura de la trama
siguiente.

Ocultación de CELT frente a libopus. tests/referencia_libopus.rs comprueba que los vectores 01, 07,
08, 09, 10 y 11 con pérdidas no se alejan de libopus más de lo medido; pruebas unitarias de
src/celt/ comprueban los núcleos x86 emulados, la predicción lineal y el periodo de un tren de
pulsos.

Secuencias de ocultación frente a libopus. La antigua plc_smoke_test (tests/conformance_rfc.rs)
exigía que la energía de cada trama SILK ocultada no superase la de la anterior, algo que libopus
1.6.1 tampoco cumple (en su salida, la cuarta de cinco pérdidas sube), y fallaba. La sustituye
la_secuencia_de_ocultacion_es_la_de_libopus (tests/referencia_libopus.rs), que decodifica sus dos
secuencias (10 paquetes, una racha de pérdidas y otros 10: del vector 07, solo CELT, con 3 pérdidas;
del 02, solo SILK, con 5) a 48 kHz en estéreo y en mono y las compara con la salida de libopus: la
de SILK es idéntica, y la de CELT no difiere más de 1, lo mismo que el vector sin pérdidas. Conserva
lo que comprobaba la anterior: que decode y decode_float dan la misma duración y que las muestras
son finitas. generar.sh construye las secuencias con secuencia_plc.c.

Remuestreador de SILK frente a libopus. Una prueba de src/silk/resampler.rs remuestrea de 16 a 12,
de 16 a 8 y de 12 a 8 kHz seis llamadas seguidas sobre el mismo estado, con longitudes que dejan una
muestra suelta al final del bucle por bloques de down_FIR, y exige las mismas muestras escritas y
los mismos valores que silk_resampler de libopus 1.6.1 (datos en tests/datos/, generados con
resto1.c).

Redundancia tras una pérdida. tests/redundancia_tras_perdida.rs decodifica, con los datos en
tests/datos/, el comienzo del primer flujo de varios con tres pérdidas seguidas de un paquete solo
SILK con redundancia CELT→SILK, y exige que la salida de ese paquete sea idéntica a la de libopus
1.6.1. Corre con cargo test, sin referencias externas. Una prueba unitaria de src/decoder.rs
comprueba la condición de mezcla con todos los modos anteriores.

Más referencias de libopus. tests/referencia_libopus.rs compara también los flujos solo SILK a 16 y
8 kHz en estéreo (idénticos, salvo el vector 12, híbrido, a 1 como mucho); los vectores con paquetes
solo CELT (01, 07, 10 y 11), solos y con pérdidas, a las 9 frecuencias y canales distintos de 48 kHz
en estéreo, con topes medidos caso a caso; un flujo de voz con FEC y 89 pasos de estéreo a mono,
solo y con pérdidas, idéntico a libopus en 8 configuraciones; y el primer flujo del multiflujo
varios como flujo normal, solo y con pérdidas, en las 10 configuraciones. Los topes de los dos
últimos grupos con pérdidas recogen dos defectos abiertos (ver defectos conocidos). generar.sh
genera todo eso con dos programas nuevos, estereo_fec.c y primer_flujo.c.

DTX con tramas de un byte. tests/referencia_libopus.rs comprueba que una trama de un byte se oculta
igual que una vacía, como en libopus.

Guion de las referencias de libopus. documentacion/infraestructura/referencia-libopus/ compila
libopus 1.6.1 (con su SHA-256 comprobado), genera las referencias que usa
tests/referencia_libopus.rs y las compara con su manifiesto, de modo que cualquiera puede pasar esas
pruebas desde un clon. Incluye un flujo SILK con DTX (casos_silk.c), uno con cambios de frecuencia
interna hecho con paquetes de los vectores 02, 03 y 04 (empalmar.c), dos secuencias de ocultación
(secuencia_plc.c), uno con pasos de estéreo a mono y FEC (estereo_fec.c), el primer flujo de un
multiflujo (primer_flujo.c) y las salidas de los flujos SILK y de los vectores con CELT a otras
frecuencias y canales.

Referencias de libopus sin aproximaciones y decodificadas con opus_decode. generar.sh compila
libopus 1.6.1 con --disable-float-approx, como lo hacen por defecto Meson y CMake (con autotools, en
x86 y ARM, celt_exp2 es un polinomio y la salida de CELT cambia), y recompila si cambian esas
opciones. Las salidas se decodifican con decodificar.c, que llama a opus_decode con la política de
pérdidas de opus_demo -d: un solo redondeo a 16 bits, al par, como decode, en lugar del doble
redondeo de opus_demo, que impedía ver diferencias de una unidad. Cambian 180 de los 390 archivos
del manifiesto (las salidas con CELT y los flujos que se codifican con libopus). Los topes de
tests/referencia_libopus.rs se han vuelto a medir con el mismo decodificador frente a las
referencias nuevas: de 173, 165 cambian y 37 suben, sobre todo en la ocultación de CELT, que ya no
se compara con un libopus distinto del que imita; el FEC pasa a ser idéntico. Las nuevas diferencias
medidas, en defectos conocidos.

Diferencia frente a libopus. tests/diferencia_libopus.rs compara los 12 vectores muestra a muestra
con su salida de referencia y falla si la diferencia máxima o la suma de diferencias de alguno pasa
de la medida. La conformidad no lo ve, porque admite un margen de calidad.

Documentación de la versión 0.2.0

Toda la documentación se ha reescrito en prosa, sin marcas de formato, y puesta al día con las
cifras medidas al cerrar la versión. Se añade una hoja de ruta (hoja-de-ruta.md) con los hitos de la
0.3.0 y lo que debe quedar demostrado para alcanzar cada uno, que el README resume. La
documentación de decode_float, en los dos decodificadores, avisa ahora de que su salida es la de 16
bits dividida entre 32 768.

Los topes de tests/diferencia_libopus.rs y las sumas de los del multiflujo en
tests/referencia_libopus.rs se han vuelto a medir: las correcciones de la versión los habían dejado
por encima de lo que el decodificador da, y una prueba con margen de sobra no avisaría de una
regresión de ese tamaño.

Versión 0.1.1, del 26 de octubre de 2026

Correcciones de la versión 0.1.1

Plegado de la primera banda en las tramas híbridas mono. Como quant_all_bands de libopus
(celt/bands.c), la banda start + 1 se pliega ahora también con un solo canal codificado; antes solo
se hacía en estéreo y, en mono, esa banda plegaba desde un historial a cero. Medido con los vectores
de RFC 8251 a 48 kHz en estéreo frente a la salida de referencia: solo cambian los vectores con
tramas híbridas mono, que se acercan a ella (suma de errores absolutos de 144 009 a 1 221 en el 05,
de 411 909 a 4 351 en el 06 y de 1 015 842 a 950 en el 12; error máximo de 67, 232 y 242 a 2, 10 y
10); los otros nueve no cambian. La red de equivalencia se regenera solo en las 28 huellas de esos
tres vectores y del multiflujo tv05+tv12.

Reinicio del canal lateral de SILK al volver de una trama solo con el canal medio. Como silk_Decode
de libopus (silk/dec_API.c), cuando una trama estéreo vuelve a traer canal lateral tras otra que
solo traía el medio, se borran la salida reciente y la memoria LPC de ese canal, el retardo de tono,
el último índice de ganancia y el tipo de señal anterior vuelven a sus valores iniciales y la trama
siguiente no interpola las NLSF. Medido igual que el anterior, ya con él aplicado: solo cambian los
vectores 05, 06 y 10, que se acercan a la referencia (suma de errores absolutos de 1 221 a 1 161 en
el 05, de 4 351 a 1 286 en el 06 y de 891 166 a 835 041 en el 10; el error máximo del 05 y el 06
baja a 1).

Reinicio de SILK al pasar de solo CELT a híbrido. libopus reinicia el decodificador SILK siempre que
el paquete anterior era solo CELT (opus_decode_frame, src/opus_decoder.c); el original solo lo hacía
si el paquete nuevo era solo SILK. Los vectores tienen 8 transiciones de solo CELT a híbrido, todas
en el 10: es el único que cambia, y su suma de errores absolutos frente a la referencia baja de 835
041 a 40 075 (error máximo de 66 a 33).

Índice de tono de la ocultación SILK, circular de verdad. plc_pitch_sample calculaba la muestra
anterior en un tono con una resta con vuelta en usize seguida del módulo, que solo da el índice
circular si la longitud de la trama guardada es potencia de dos; con tramas de 160, 240 o 320
muestras leía otra muestra cuando la posición era menor que el tono. Ahora el índice es circular
para cualquier longitud (prueba con oráculo, que fallaba con el código anterior). Solo afecta a la
ocultación de pérdidas; los vectores de RFC 8251 no tienen pérdidas y su salida no cambia.

Primera trama de cada paquete SILK siempre independiente. Como silk_Decode de libopus, la primera
trama del paquete se decodifica como CODE_INDEPENDENTLY también en el canal lateral cuando la trama
anterior solo traía el medio; antes se usaba CODE_INDEPENDENTLY_NO_LTP_SCALING y, si la trama era
sonora, no se leía el índice de escala LTP y el resto del paquete se leía desplazado. En los
vectores el caso ocurre 4 veces, siempre en tramas sordas, y su salida no cambia; sí ocurre en
tramas sonoras con paquetes dañados.

silk_log2lin trunca como libopus. En la rama de entradas menores que 2048 (16 en Q7), libopus suma
(out * curva) >> 7 sin redondeo (silk_ADD_RSHIFT32); aquí se redondeaba. Comprobado con un oráculo
de la fórmula de libopus en todas las entradas de −1 a 4000, que fallaba con el código anterior. El
único llamador nunca entra en esa rama: la salida no cambia.

Cota de la ganancia al desnormalizar las bandas de CELT, como libopus. libopus limita por arriba el
logaritmo de la ganancia (celt_exp2_db(MIN32(32.f, lg)), que en coma flotante es celt_exp2); aquí se
limitaba por abajo, a −32, y por arriba la ganancia podía crecer sin límite. Medido: en los 12
vectores, con pérdidas y con paquetes dañados, ninguna banda sale de ±32 (extremos 28,5 y −24,5),
así que la salida no cambia.

Pruebas de la versión 0.1.1

La conformidad con RFC 8251 exige final_range. Antes solo avisaba si el estado final del
decodificador de rango de un paquete no coincidía con el de referencia, y el aviso ni se veía cuando
la prueba pasaba. Medido: quitar la mezcla del rango de la trama CELT redundante en las tramas solo
SILK cambia el final_range de 14 paquetes (vectores 08, 09 y 12) sin tocar el PCM, y la prueba
anterior seguía en verde; ahora falla, igual que si se quita en las tramas híbridas (vector 10). Con
el decodificador actual coincide en todos los paquetes de los 12 vectores.

La red de equivalencia cubre las transiciones de solo CELT a solo SILK. Los vectores de RFC 8251 no
tienen ninguna; el caso nuevo transiciones tv01+tv02 alterna bloques de 50 paquetes de un vector
solo CELT y otro solo SILK. Sin el reinicio de SILK en esa transición, la red se pone en rojo. Las
otras 110 huellas no cambian.

Diseño de la versión 0.1.1

La cola de redundancia de las tramas solo SILK se lee en src/decoder.rs, junto a la de las híbridas,
como en opus_decode_frame de libopus (RFC 6716 §4.5.1). SILK ya no depende del decodificador de
paquetes. El orden de lectura no cambia (carga SILK, cola, cierre), así que la salida y el
final_range son idénticos.

Versión 0.1.0, del 26 de octubre de 2026

Correcciones de la versión 0.1.0

Eliminados los registros de depuración que escribían en disco. La 0.1.1 contiene 29 regiones #region
agent log que abren, en modo de añadir, un archivo de registro (22 con rutas absolutas de la máquina
del autor bajo /Users/tadeusz/Opus/Rasopus/.cursor/, 7 con la ruta relativa
.cursor/debug-bea564.log). Dos se ejecutan en cada trama CELT. Se quitan las 29
(parches/0001-quitar-registros-de-depuracion.patch
(../parches/0001-quitar-registros-de-depuracion.patch)); la salida del decodificador no cambia
(conformidad RFC 8251 12/12 y red de equivalencia idéntica). Avisado al autor en el issue #8
(https://github.com/TadeuszWolfGang/Rusopus/issues/8).

Proyecto de la versión 0.1.0

El crate pasa a llamarse copus_decoder, con licencia GPL-3.0 o posterior sobre la rama Apache-2.0
del original; se conservan sus avisos y el de libopus en licencias/.

Se quita la dependencia de desarrollo criterion, que solo usaban bancos no incluidos en el paquete.

Red de equivalencia: tests/red_equivalencia.rs y red/huellas.txt (110 huellas generadas con la 0.1.1
original).

Documentación en español y scripts de preparación del entorno con descarga verificada de los
vectores de RFC 8251.

Auditoría de la versión 0.1.0

En curso. Cada cambio de esta sección deja la red de equivalencia y la conformidad idénticas.

Formato uniforme con rustfmt; cargo fmt --check queda limpio.

Las pruebas de conformidad ya no escriben en disco: fuera las escrituras a un registro con ruta
absoluta de la máquina del autor original, el volcado a /tmp/rust_tv07.pcm y las ocho variables de
entorno de trazado.

Fuera el código muerto de depuración de src/: la macro debug_trace!, las funciones de registro, los
contadores de paquete que solo servían para trazar, una FFT de referencia sin usar y los
allow(dead_code) que lo ocultaban. La DFT directa que la sustituye en las pruebas tiene su propia
prueba unitaria.

Fuera tres conversiones al mismo tipo; cargo clippy --all-targets --all-features -- -D warnings
queda sin avisos.

El comparador de salidas, que solo usan las pruebas, pasa de src/compare.rs a tests/compare/.

Nueva prueba de robustez, tests/no_panico.rs: paquetes aleatorios, con TOC válido, truncados y con
bits alterados, para todas las frecuencias y canales y en multiflujo. El decodificador debe devolver
un error, nunca entrar en pánico. Encontró dos defectos, ya corregidos:

Con 16 bloques MDCT (tramas de 20 ms transitorias con cierto ajuste tiempo-frecuencia) la máscara de
colapso de CELT se desbordaba. Pasa a 32 bits, como en libopus.

Un paquete híbrido que anunciaba más redundancia de la que cabía en la trama hacía entrar en pánico
al decodificador. Ahora se descarta la redundancia, como hace libopus.

No queda ningún panic!, unreachable!, unwrap() ni assert! fuera de las pruebas: las invariantes del
orden LPC, la periodicidad LTP y la forma del tono las garantiza el tipo. Quedan 12 debug_assert!,
que solo actúan en compilación de depuración.

Las pruebas encuentran los vectores de RFC 8251 sin configurar nada: por defecto los buscan en la
carpeta que crean los scripts de preparación (OPUS_TESTVECTORS_DIR sigue sirviendo para usar otra).
Antes apuntaban a ../../testdata, una ruta del espacio de trabajo del autor original.

Sin números mágicos en la lógica del decodificador: los valores con significado (frecuencias,
duraciones, bandas, parámetros del decodificador de rango, de SILK y de CELT, valores iniciales del
estado) son constantes con nombre, documentadas con su apartado de RFC 6716 o su función de libopus.
Solo quedan como literal los desplazamientos de formato de coma fija, los índices de filtros
desenrollados y los números que el propio RFC usa como nombre.

Fuera las copias de un mismo valor o cálculo: una sola banda final de CELT por ancho de banda, una
sola BITRES, un solo tipo de señal sonora de SILK y una sola lista de duraciones de trama. Las
funciones reescritas tienen pruebas que las comparan con la versión anterior en todo su dominio.

Los valores de la ocultación de pérdidas de SILK que no proceden de libopus quedan marcados en el
código como no comprobados.

Módulos de una sola responsabilidad, sin cambiar la salida (red de equivalencia y conformidad
idénticas tras cada paso):

lib.rs queda como API pública; el decodificador de un flujo pasa a decoder.rs, partido por modos, y
OpusDecoder ya no copia su estado.

CELT: la decodificación de una trama, la ocultación, el recorrido de bandas (uno solo para mono y
estéreo), la asignación de bits (rate.rs) y la síntesis MDCT (celt/synthesis.rs) van por etapas; un
solo postfiltro por canal y un tipo CeltOutput en lugar de un bool.

SILK: el estado de cada canal se agrupa en SilkChannel, el remuestreo tiene un solo camino y la
síntesis, la lectura de índices y la de pulsos van por etapas.

Una sola copia de las primitivas de coma fija de SILK, del libro NLSF, de ec_ilog y de la saturación
a i16 (pcm.rs); el multiflujo comparte un solo camino para i16 y f32.

parse_packet va por etapas, una por código de empaquetado.

Ninguna función de src/ pasa de 100 líneas. El único allow de clippy de módulo que queda es
excessive_precision en celt/modes.rs, cuyas tablas copian los literales de libopus.

Pruebas unitarias del reinicio estéreo al pasar de mono a estéreo y de la entrada de una trama
ocultada en la historia de SILK, conductas que la red de equivalencia no ejercita.

Nueva barrera en tests/multistream.rs: la salida del multiflujo debe ser la de un decodificador por
flujo repartida según el mapa de canales, en i16 y en f32.

Toda la documentación del código, en español. Los mensajes de error (Display) se conservan en
inglés, como en la versión original.

Las diferencias con libopus y las decisiones de diseño abiertas que se han encontrado quedan en
defectos conocidos (defectos-conocidos.md).
