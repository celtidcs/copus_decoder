/* Creación de los archivos de salida de los programas de referencia-libopus.
 * fopen(ruta, "wb") crea el archivo con permisos 0666 recortados por la máscara del proceso: con
 * una máscara permisiva, cualquiera podría escribir en las referencias. crear_salida lo crea con
 * permisos 0644 explícitos (lectura para todos, escritura solo para el dueño), sea cual sea la
 * máscara, y si ya existe lo vacía, como "wb". Devuelve NULL y deja errno si falla.
 */
#ifndef SALIDA_H
#define SALIDA_H

#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

/* Permisos de las salidas: lectura y escritura para el dueño, solo lectura para los demás. */
#define PERMISOS_SALIDA (S_IRUSR | S_IWUSR | S_IRGRP | S_IROTH)

static FILE *crear_salida(const char *ruta) {
    int fd = open(ruta, O_WRONLY | O_CREAT | O_TRUNC, PERMISOS_SALIDA);
    if (fd < 0) return NULL;
    /* open no aplica los permisos a un archivo que ya existía: se fijan siempre. */
    if (fchmod(fd, PERMISOS_SALIDA) != 0) { close(fd); return NULL; }
    FILE *f = fdopen(fd, "wb");
    if (!f) close(fd);
    return f;
}

#endif
