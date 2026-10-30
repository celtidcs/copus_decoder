/* Empalma por bloques los paquetes de varios flujos .bit de opus_demo en uno solo.
 *
 * Uso: empalmar <bloque> <salida.bit> <entrada1.bit> <entrada2.bit> [...]
 *
 * El paquete n de la salida es el paquete n de la entrada (n / bloque) % entradas, hasta agotar la
 * más corta. Con los vectores SILK de RFC 8251 de banda estrecha, media y ancha (02, 03 y 04), la
 * frecuencia interna del decodificador cambia en cada empalme (8, 12 y 16 kHz), con todas las
 * duraciones de trama y con paquetes mono y estéreo. Cada registro conserva su longitud, su
 * final_range y su carga: el estado del codificador de rango es propio de cada paquete, así que
 * opus_demo -d lo sigue comprobando.
 */
#include <stdio.h>
#include <stdlib.h>

#define MAX_ENTRADAS 8
/* Carga máxima que se acepta en un registro (ver perdidas.c). */
#define MAX_CARGA 65536

static int leer_u32(FILE *f, unsigned long *v) {
    unsigned char b[4];
    if (fread(b, 1, 4, f) != 4) return 0;
    *v = ((unsigned long)b[0] << 24) | ((unsigned long)b[1] << 16) | ((unsigned long)b[2] << 8) | b[3];
    return 1;
}

static void escribir_u32(FILE *f, unsigned long v) {
    unsigned char b[4] = {(unsigned char)(v >> 24), (unsigned char)(v >> 16),
                          (unsigned char)(v >> 8), (unsigned char)v};
    fwrite(b, 1, 4, f);
}

/* Lee el siguiente registro de f en carga. Devuelve 1 si lo hay, 0 al final y -1 si está dañado. */
static int leer_registro(FILE *f, unsigned long *largo, unsigned long *rango, unsigned char *carga) {
    if (!leer_u32(f, largo)) return 0;
    if (!leer_u32(f, rango) || *largo > MAX_CARGA || fread(carga, 1, *largo, f) != *largo) return -1;
    return 1;
}

int main(int argc, char **argv) {
    static unsigned char cargas[MAX_ENTRADAS][MAX_CARGA];
    unsigned long largos[MAX_ENTRADAS], rangos[MAX_ENTRADAS];
    FILE *entradas[MAX_ENTRADAS], *sal;
    int nentradas = argc - 3, bloque, i;
    long n = 0;
    if (argc < 5 || nentradas > MAX_ENTRADAS || (bloque = atoi(argv[1])) <= 0) {
        fprintf(stderr, "uso: empalmar <bloque> <salida.bit> <entrada1.bit> <entrada2.bit> [...]\n");
        return 1;
    }
    for (i = 0; i < nentradas; i++) {
        entradas[i] = fopen(argv[3 + i], "rb");
        if (!entradas[i]) { perror(argv[3 + i]); return 1; }
    }
    sal = fopen(argv[2], "wb");
    if (!sal) { perror(argv[2]); return 1; }
    for (;; n++) {
        int elegida = (int)((n / bloque) % nentradas), fin = 0;
        /* Todas las entradas avanzan a la vez, para que el paquete n venga de la posición n; no se
           escribe nada hasta saber que ninguna se ha acabado. */
        for (i = 0; i < nentradas; i++) {
            int r = leer_registro(entradas[i], &largos[i], &rangos[i], cargas[i]);
            if (r < 0) { fprintf(stderr, "%s: registro %ld dañado\n", argv[3 + i], n); return 1; }
            if (r == 0) fin = 1;
        }
        if (fin) break;
        escribir_u32(sal, largos[elegida]);
        escribir_u32(sal, rangos[elegida]);
        fwrite(cargas[elegida], 1, largos[elegida], sal);
    }
    for (i = 0; i < nentradas; i++) fclose(entradas[i]);
    if (fclose(sal) != 0) { perror(argv[2]); return 1; }
    printf("%s: %ld paquetes\n", argv[2], n);
    return 0;
}
