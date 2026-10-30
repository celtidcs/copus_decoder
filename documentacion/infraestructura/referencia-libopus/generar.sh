#!/usr/bin/env bash
# Genera las referencias de libopus 1.6.1 que usan las pruebas de tests/referencia_libopus.rs:
# flujos con pérdidas, con FEC en banda, con DTX y con cambios de banda decodificados con
# decodificar.c (la API opus_decode con la política de pérdidas de opus_demo -d; a 48 kHz en
# estéreo y, los de SILK y los vectores con CELT, también a otras frecuencias y canales), dos secuencias de ocultación (también a 48 kHz en mono), un flujo con
# pasos de estéreo a mono y FEC, y flujos multiflujo codificados y decodificados con la API
# multiflujo, con el primero de sus flujos también como flujo normal.
#
# Uso (desde cualquier carpeta):
#   bash documentacion/infraestructura/referencia-libopus/generar.sh
#
# Necesita gcc, make, curl, tar y sha256sum, y los vectores de RFC 8251 que descarga
# preparar-entorno.sh. Variables opcionales:
#   OPUS_TESTVECTORS_DIR         carpeta de los vectores (por defecto, la de preparar-entorno.sh).
#   OPUS_REFERENCIA_LIBOPUS_DIR  carpeta de salida (por defecto, la que buscan las pruebas).
#
# Es idempotente: si la salida ya coincide con SHA256SUMS, no regenera nada. libopus se compila
# en target/referencia-libopus, que git ignora, y se reutiliza en las ejecuciones siguientes.
set -euo pipefail

GUION_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
RAIZ=$(cd "$GUION_DIR/../../.." && pwd)
VECTORES=${OPUS_TESTVECTORS_DIR:-$RAIZ/material_secundario/vectores/opus_newvectors}
DESTINO=${OPUS_REFERENCIA_LIBOPUS_DIR:-$RAIZ/material_secundario/vectores/referencia-libopus-1.6.1}
TRABAJO=$RAIZ/target/referencia-libopus
MANIFIESTO=$GUION_DIR/SHA256SUMS

# libopus 1.6.1 oficial; el SHA-256 es el de https://downloads.xiph.org/releases/opus/SHA256SUMS.txt
VERSION=opus-1.6.1
TARBALL=$VERSION.tar.gz
URL=https://downloads.xiph.org/releases/opus/$TARBALL
SHA_TARBALL=6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1
# libopus sin las aproximaciones rápidas de coma flotante (FLOAT_APPROX), como lo compilan por
# defecto Meson y CMake; autotools las activa por defecto en x86 y ARM, y con ellas celt_exp2 es
# un polinomio y la salida de CELT cambia. Este decodificador calcula como libopus sin ellas.
OPCIONES_CONFIGURE=--disable-float-approx

# Patrones de pérdida de perdidas.c y casos de multiflujo.c.
PATRONES="p9 r3"
CASOS_MULTIFLUJO="audio128 voip24 audio64_60ms cbr48 fec varios"
# Flujo con FEC en banda: voz del vector 02, 32 kb/s, 20 % de pérdida declarada al codificador.
TASA_FEC=32000
PERDIDA_DECLARADA_FEC=20
# Frecuencias y canales de salida, además de 48 kHz en estéreo, de los flujos SILK: los de la red
# de equivalencia (tests/red_equivalencia.rs) y 16 y 8 kHz en estéreo.
FRECUENCIAS="48000:1 24000:2 16000:2 16000:1 12000:2 8000:2 8000:1"
# Todas las demás frecuencias y canales de salida, para los flujos con CELT.
FRECUENCIAS_TODAS="48000:1 24000:2 24000:1 16000:2 16000:1 12000:2 12000:1 8000:2 8000:1"
# Paquetes seguidos de cada vector en el flujo empalmado; 7 es primo con el periodo 9 del patrón
# p9, así que hay pérdidas en todas las posiciones respecto a un empalme, también justo después.
BLOQUE_EMPALMES=7
# Vectores de RFC 8251 con SILK cuyas salidas se generan también a esas frecuencias.
VECTORES_SILK="02 03 04 12"
# Vectores de RFC 8251 con paquetes solo CELT cuyas salidas se generan a FRECUENCIAS_TODAS.
VECTORES_CELT="01 07 10 11"
# Secuencias de ocultación: PAQUETES_PLC paquetes, una racha de pérdidas y otros PAQUETES_PLC, del
# vector 07 (solo CELT, 3 pérdidas) y del 02 (solo SILK, 5 pérdidas), como nombre:vector:pérdidas.
PAQUETES_PLC=10
SECUENCIAS_PLC="plc_celt:07:3 plc_silk:02:5"

fallo() { echo "ERROR: $*" >&2; exit 1; }

# 1. Herramientas y datos de entrada.
for herramienta in gcc make curl tar sha256sum; do
    command -v "$herramienta" > /dev/null || fallo "falta $herramienta"
done
for k in 01 02 03 04 05 06 07 08 09 10 11 12; do
    [ -f "$VECTORES/testvector$k.bit" ] && [ -f "$VECTORES/testvector$k.dec" ] \
        || fallo "faltan los vectores de RFC 8251 en $VECTORES (ejecuta preparar-entorno.sh)"
done

# Comprueba la salida contra el manifiesto; con --quiet solo imprime los archivos distintos.
coincide_con_manifiesto() {
    [ -d "$DESTINO" ] && (cd "$DESTINO" && sha256sum -c --quiet "$MANIFIESTO" 2> /dev/null)
}

if coincide_con_manifiesto > /dev/null; then
    echo "Las referencias de $DESTINO ya coinciden con SHA256SUMS; no hay nada que hacer."
    exit 0
fi

# 2. Descarga de libopus, con el SHA-256 comprobado.
mkdir -p "$TRABAJO"
cd "$TRABAJO"
if [ ! -f "$TARBALL" ]; then
    echo "== Descargando $URL"
    curl -sSfL -o "$TARBALL.part" "$URL"
    mv "$TARBALL.part" "$TARBALL"
fi
echo "$SHA_TARBALL  $TARBALL" | sha256sum -c --quiet \
    || { rm -f "$TARBALL"; fallo "el SHA-256 de $TARBALL no coincide (borrado; vuelve a ejecutar)"; }

# 3. Compilación de libopus y de los programas auxiliares. Se recompila si cambian las opciones
# de configure (las de una compilación anterior quedan en $VERSION/.opciones_configure).
if [ ! -x "$VERSION/opus_demo" ] || [ ! -f "$VERSION/.libs/libopus.a" ] \
    || [ "$(cat "$VERSION/.opciones_configure" 2> /dev/null)" != "$OPCIONES_CONFIGURE" ]; then
    echo "== Compilando $VERSION ($OPCIONES_CONFIGURE)"
    rm -rf "$VERSION"
    tar xzf "$TARBALL"
    (cd "$VERSION" && ./configure $OPCIONES_CONFIGURE > ../configure.log 2>&1 \
        && make -j"$(nproc)" > ../make.log 2>&1) \
        || fallo "falló la compilación de libopus (ver $TRABAJO/configure.log y make.log)"
    echo "$OPCIONES_CONFIGURE" > "$VERSION/.opciones_configure"
fi
OPUS_DEMO=$TRABAJO/$VERSION/opus_demo
for programa in perdidas multiflujo lbrr casos_silk empalmar secuencia_plc estereo_fec \
    primer_flujo decodificar; do
    gcc -O2 -Wall -I "$VERSION/include" "$GUION_DIR/$programa.c" "$VERSION/.libs/libopus.a" -lm \
        -o "$programa"
done

# 4. Generación.
mkdir -p "$DESTINO/flujos" "$DESTINO/salidas" "$DESTINO/multiflujo" "$DESTINO/frecuencias"
FLUJOS=$DESTINO/flujos
SALIDAS=$DESTINO/salidas
FRECUENCIAS_DIR=$DESTINO/frecuencias
REGISTRO=$TRABAJO/decodificar.log
: > "$REGISTRO"

# <flujo.bit> <salida.dec> [frecuencia canales]: decodificar.c, por defecto a 48000 2. No se usa
# opus_demo -d: escribe con opus_decode24 y (s + 128) >> 8, dos redondeos, y las pruebas comparan
# la salida i16 de OpusDecoder::decode, que como opus_decode redondea una vez.
decodificar() {
    "$TRABAJO/decodificar" "${3:-48000}" "${4:-2}" "$1" "$2" 2>> "$REGISTRO" \
        || fallo "decodificar $1"
}

# <flujo.bit> <frecuencia:canales>...: salidas en frecuencias/<flujo>_<frecuencia>_<canales>.dec
decodificar_en() {
    local flujo=$1 salida
    shift
    for salida in "$@"; do
        decodificar "$flujo" \
            "$FRECUENCIAS_DIR/$(basename "$flujo" .bit)_${salida%:*}_${salida#*:}.dec" \
            "${salida%:*}" "${salida#*:}"
    done
}

# <flujo.bit>: sus versiones con pérdidas, con los patrones de PATRONES.
con_perdidas() {
    local patron
    for patron in $PATRONES; do
        ./perdidas "$1" "$patron" "${1%.bit}_$patron.bit"
    done
}

echo "== Flujo con FEC en banda"
"$OPUS_DEMO" -e voip 48000 2 "$TASA_FEC" -inbandfec -loss "$PERDIDA_DECLARADA_FEC" \
    "$VECTORES/testvector02.dec" "$FLUJOS/fec_$TASA_FEC.bit" > /dev/null 2>> "$REGISTRO" \
    || fallo "opus_demo -e"

echo "== Flujo SILK con DTX"
./casos_silk "$VECTORES/testvector02.dec" "$FLUJOS/dtx.bit"

echo "== Flujo con cambios de frecuencia interna (vectores 02, 03 y 04 empalmados)"
./empalmar "$BLOQUE_EMPALMES" "$FLUJOS/empalmes.bit" \
        "$VECTORES/testvector02.bit" "$VECTORES/testvector03.bit" "$VECTORES/testvector04.bit"

echo "== Secuencias de ocultación"
for secuencia in $SECUENCIAS_PLC; do
    IFS=: read -r nombre vector perdidas <<< "$secuencia"
    ./secuencia_plc "$VECTORES/testvector$vector.bit" "$PAQUETES_PLC" "$perdidas" \
        "$FLUJOS/$nombre.bit"
done

echo "== Flujos con pérdidas"
for patron in $PATRONES; do
    for k in 01 02 03 04 05 06 07 08 09 10 11 12; do
        ./perdidas "$VECTORES/testvector$k.bit" "$patron" "$FLUJOS/testvector${k}_$patron.bit"
    done
    for flujo in "fec_$TASA_FEC" dtx empalmes; do
        ./perdidas "$FLUJOS/$flujo.bit" "$patron" "$FLUJOS/${flujo}_$patron.bit"
    done
done

echo "== Salidas de decodificar"
for k in 01 02 03 04 05 06 07 08 09 10 11 12; do
    decodificar "$VECTORES/testvector$k.bit" "$SALIDAS/testvector$k.dec"
done
for flujo in "$FLUJOS"/*.bit; do
    decodificar "$flujo" "$SALIDAS/$(basename "$flujo" .bit).dec"
done

echo "== Salidas de decodificar a otras frecuencias"
FLUJOS_SILK=""
for k in $VECTORES_SILK; do
    FLUJOS_SILK="$FLUJOS_SILK $VECTORES/testvector$k.bit"
    for patron in $PATRONES; do
        FLUJOS_SILK="$FLUJOS_SILK $FLUJOS/testvector${k}_$patron.bit"
    done
done
for flujo in "$FLUJOS"/dtx*.bit "$FLUJOS"/empalmes*.bit; do
    FLUJOS_SILK="$FLUJOS_SILK $flujo"
done
for flujo in $FLUJOS_SILK; do
    decodificar_en "$flujo" $FRECUENCIAS
done
for k in $VECTORES_CELT; do
    decodificar_en "$VECTORES/testvector$k.bit" $FRECUENCIAS_TODAS
    for patron in $PATRONES; do
        decodificar_en "$FLUJOS/testvector${k}_$patron.bit" $FRECUENCIAS_TODAS
    done
done

for secuencia in $SECUENCIAS_PLC; do
    decodificar_en "$FLUJOS/${secuencia%%:*}.bit" 48000:1
done

echo "== Paquetes con LBRR (opus_packet_has_lbrr)"
for flujo in "$FLUJOS"/fec_*.bit; do
    ./lbrr "$flujo"
done

echo "== Multiflujo"
for caso in $CASOS_MULTIFLUJO; do
    ./multiflujo "$caso" "$SALIDAS/testvector02.dec" "$SALIDAS/testvector01.dec" \
        "$DESTINO/multiflujo/ms_$caso.bit" "$DESTINO/multiflujo/ms_$caso.dec"
done

# Estos dos flujos parten de salidas de libopus generadas arriba, y se decodifican aparte.
echo "== Flujo con pasos de estéreo a mono y FEC (salida del vector 02)"
./estereo_fec "$SALIDAS/testvector02.dec" "$FLUJOS/estereo_fec.bit"
con_perdidas "$FLUJOS/estereo_fec.bit"

echo "== Primer flujo del multiflujo varios"
./primer_flujo "$DESTINO/multiflujo/ms_varios.bit" "$FLUJOS/varios_f0.bit"
con_perdidas "$FLUJOS/varios_f0.bit"

for flujo in "$FLUJOS"/estereo_fec*.bit "$FLUJOS"/varios_f0*.bit; do
    decodificar "$flujo" "$SALIDAS/$(basename "$flujo" .bit).dec"
done
for flujo in "$FLUJOS"/estereo_fec*.bit; do
    decodificar_en "$flujo" $FRECUENCIAS
done
for flujo in "$FLUJOS"/varios_f0*.bit; do
    decodificar_en "$flujo" $FRECUENCIAS_TODAS
done

# 5. Comprobación final contra el manifiesto.
echo "== Comprobando contra SHA256SUMS"
if ! coincide_con_manifiesto; then
    cat >&2 << EOF
ERROR: los archivos de arriba no coinciden con SHA256SUMS.
Las referencias se generaron en Linux x86-64 con libopus configurado con $OPCIONES_CONFIGURE,
que elige las instrucciones SSE/AVX al ejecutarse; en otra CPU o con otro compilador la salida puede
ser distinta, y las pruebas de tests/referencia_libopus.rs pueden fallar con ella.
EOF
    exit 1
fi

cat << EOF
Referencias generadas en $DESTINO y comprobadas con SHA256SUMS.
Para pasar las pruebas contra libopus:
  cargo test --release --test referencia_libopus -- --ignored
EOF
if [ -n "${OPUS_REFERENCIA_LIBOPUS_DIR:-}" ]; then
    echo "(con OPUS_REFERENCIA_LIBOPUS_DIR=$OPUS_REFERENCIA_LIBOPUS_DIR definida)"
fi
