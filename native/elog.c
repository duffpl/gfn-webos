/* SPDX-License-Identifier: GPL-3.0-or-later */
#include <stdio.h>

FILE *elogf = NULL;

int gfn_elog_open(const char *path)
{
    if (elogf) return 1;
    elogf = path ? fopen(path, "a") : stderr;
    if (!elogf) return 0;
    setvbuf(elogf, NULL, _IOLBF, 0);
    return 1;
}
