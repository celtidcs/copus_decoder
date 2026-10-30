/* Extrae el primer flujo de un flujo multiflujo .bit de opus_demo como un flujo normal.
 *
 * Uso: primer_flujo <multiflujo.bit> <salida.bit>
 *
 * En un paquete multiflujo, todos los subpaquetes menos el último van en formato autodelimitado
 * (RFC 6716, anexo B): tras el TOC, el byte de cuenta, el relleno y las demás longitudes, llevan
 * la longitud de la última trama (en CBR, la de cada trama). Este programa quita esa longitud
 * del primer subpaquete y lo escribe en formato normal, con el relleno que tuviera. El estado del
 * codificador de rango de cada registro se escribe a 0, porque el del multiflujo es el del
 * paquete entero; con 0, opus_demo -d no lo comprueba.
 */
#include <stdio.h>
#include <stdlib.h>
#include "salida.h"

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

/* Longitud de trama de RFC 6716, §3.2.1, en p[i]: uno o dos bytes. Guarda en *bytes cuántos
   ocupa. Devuelve -1 si se sale del paquete. */
static long longitud(const unsigned char *p, unsigned long n, unsigned long i, int *bytes) {
    if (i >= n) return -1;
    if (p[i] < 252) { *bytes = 1; return p[i]; }
    if (i + 1 >= n) return -1;
    *bytes = 2;
    return p[i] + 4L * p[i + 1];
}

/* Escribe en sal el primer subpaquete de p (n bytes) en formato normal. Devuelve 0 o -1 si el
   paquete está dañado. */
static int escribir_primer_flujo(FILE *sal, const unsigned char *p, unsigned long n) {
    unsigned long cabecera, datos = 0, relleno = 0, i;
    long l, l2, ultima;
    int b, b1, codigo, cuenta;
    if (n == 0) return -1;
    codigo = p[0] & 3;
    if (codigo == 0 || codigo == 1) {
        if ((l = longitud(p, n, 1, &b)) < 0) return -1;
        cabecera = 1;
        datos = codigo == 0 ? (unsigned long)l : 2UL * (unsigned long)l;
    } else if (codigo == 2) {
        if ((l = longitud(p, n, 1, &b1)) < 0 || (l2 = longitud(p, n, 1 + b1, &b)) < 0) return -1;
        cabecera = 1 + b1;
        datos = (unsigned long)(l + l2);
    } else {
        if (n < 2) return -1;
        cuenta = p[1] & 0x3f;
        i = 2;
        if (p[1] & 0x40) {
            int byte;
            do {
                if (i >= n) return -1;
                byte = p[i++];
                relleno += byte == 255 ? 254 : byte;
            } while (byte == 255);
        }
        if (p[1] & 0x80) {
            int k;
            for (k = 0; k < cuenta - 1; k++) {
                if ((l = longitud(p, n, i, &b)) < 0) return -1;
                i += b;
                datos += l;
            }
        }
        cabecera = i;
        if ((ultima = longitud(p, n, i, &b)) < 0) return -1;
        datos += p[1] & 0x80 ? (unsigned long)ultima : (unsigned long)ultima * cuenta;
        datos += relleno;
    }
    if (cabecera + b + datos > n) return -1;
    escribir_u32(sal, cabecera + datos);
    escribir_u32(sal, 0);
    fwrite(p, 1, cabecera, sal);
    fwrite(p + cabecera + b, 1, datos, sal);
    return 0;
}

int main(int argc, char **argv) {
    static unsigned char carga[MAX_CARGA];
    unsigned long largo, rango;
    long paquetes = 0;
    FILE *ent, *sal;
    if (argc != 3) {
        fprintf(stderr, "uso: primer_flujo <multiflujo.bit> <salida.bit>\n");
        return 1;
    }
    ent = fopen(argv[1], "rb");
    if (!ent) { perror(argv[1]); return 1; }
    sal = crear_salida(argv[2]);
    if (!sal) { perror(argv[2]); return 1; }
    while (leer_u32(ent, &largo)) {
        if (!leer_u32(ent, &rango) || largo > MAX_CARGA || fread(carga, 1, largo, ent) != largo
            || escribir_primer_flujo(sal, carga, largo) != 0) {
            fprintf(stderr, "%s: registro %ld dañado\n", argv[1], paquetes);
            return 1;
        }
        paquetes++;
    }
    fclose(ent);
    if (fclose(sal) != 0) { perror(argv[2]); return 1; }
    printf("%s: %ld paquetes\n", argv[2], paquetes);
    return 0;
}
