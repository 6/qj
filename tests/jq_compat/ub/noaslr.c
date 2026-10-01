// noaslr PROGRAM ARGS...: exec PROGRAM with ASLR off (macOS), like
// `setarch -R` on Linux. posix_spawn with POSIX_SPAWN_SETEXEC replaces this
// process, so the caller sees PROGRAM's own exit status or signal.
// argv[1] is the path; argv[2] becomes the new argv[0].
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>

#ifndef _POSIX_SPAWN_DISABLE_ASLR
#define _POSIX_SPAWN_DISABLE_ASLR 0x0100
#endif

extern char **environ;

int main(int argc, char **argv) {
  if (argc < 3) {
    fprintf(stderr, "usage: noaslr PATH ARGV0 [ARGS...]\n");
    return 2;
  }
  posix_spawnattr_t attr;
  posix_spawnattr_init(&attr);
  posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETEXEC | _POSIX_SPAWN_DISABLE_ASLR);
  pid_t pid;
  int rc = posix_spawn(&pid, argv[1], NULL, &attr, argv + 2, environ);
  fprintf(stderr, "noaslr: posix_spawn: %s\n", strerror(rc));
  return 127;
}
