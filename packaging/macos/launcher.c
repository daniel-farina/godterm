/* GodTerm.app's main executable: runs Contents/Resources/launch.sh, which
 * opens iTerm (or Terminal) running the bundled godterm. A Mach-O main
 * executable keeps the bundle's code signature and hardened runtime
 * ordinary; the work stays in the readable shell script. */
#include <libgen.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char exe[PATH_MAX], real[PATH_MAX], script[PATH_MAX];
    uint32_t n = sizeof exe;
    if (_NSGetExecutablePath(exe, &n) != 0 || !realpath(exe, real)) {
        fprintf(stderr, "GodTerm: cannot find its own path\n");
        return 1;
    }
    /* .../GodTerm.app/Contents/MacOS/GodTerm -> .../Contents/Resources/launch.sh */
    char *macos = dirname(real);
    snprintf(script, sizeof script, "%s/../Resources/launch.sh", macos);
    char *args[] = {"/bin/sh", script, NULL};
    (void)argc;
    (void)argv;
    execv("/bin/sh", args);
    perror("GodTerm: exec /bin/sh");
    return 1;
}
