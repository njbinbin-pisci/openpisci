/* pisci-xi-helper: X11 mouse position helper for VMware+Xorg environments.
 *
 * In VMware with Xorg, xdotool mousemove only moves the XTEST slave pointer
 * (device id=4), which is decoupled from the visible cursor and the XInput2
 * master pointer (device id=2). This helper calls XIWarpPointer on the master
 * pointer, which correctly positions the X11 pointer for event delivery.
 *
 * Usage:
 *   pisci-xi-helper move <x> <y>
 *     Move the master pointer to (x, y).
 *
 *   pisci-xi-helper drag <sx> <sy> <ex> <ey> [steps]
 *     Smooth drag from (sx,sy) to (ex,ey) with intermediate steps.
 *     Steps default to 20 if not specified.
 *     Uses XTestFakeMotionEvent to generate MotionNotify events between
 *     mousedown and mouseup, which is required for apps to detect the drag.
 */

#include <X11/Xlib.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static Display *open_display(void) {
    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) {
        fprintf(stderr, "Cannot open display\n");
        return NULL;
    }

    /* Verify XInput2 is available */
    int xi_opcode, xi_event, xi_error;
    if (!XQueryExtension(dpy, "XInputExtension", &xi_opcode, &xi_event, &xi_error)) {
        fprintf(stderr, "XInputExtension not available\n");
        XCloseDisplay(dpy);
        return NULL;
    }
    return dpy;
}

static void warp_pointer(Display *dpy, int x, int y) {
    XIWarpPointer(dpy, 2, None, DefaultRootWindow(dpy),
                  0, 0, 0, 0, x, y);
    XFlush(dpy);
}

static void fake_motion(Display *dpy, int x, int y) {
    XTestFakeMotionEvent(dpy, -1, x, y, CurrentTime);
    XFlush(dpy);
}

static void fake_button(Display *dpy, int button, int press) {
    if (press) {
        XTestFakeButtonEvent(dpy, button, True, CurrentTime);
    } else {
        XTestFakeButtonEvent(dpy, button, False, CurrentTime);
    }
    XFlush(dpy);
}

static int cmd_move(int argc, char *argv[]) {
    if (argc < 4) {
        fprintf(stderr, "Usage: %s move <x> <y>\n", argv[0]);
        return 1;
    }
    int x = atoi(argv[2]);
    int y = atoi(argv[3]);

    Display *dpy = open_display();
    if (!dpy) return 1;

    warp_pointer(dpy, x, y);
    /* Also update XTEST pointer so XQueryPointer returns correct position */
    fake_motion(dpy, x, y);

    printf("XIWarpPointer: moved master pointer (id=2) to (%d,%d)\n", x, y);
    XCloseDisplay(dpy);
    return 0;
}

static int cmd_drag(int argc, char *argv[]) {
    if (argc < 6) {
        fprintf(stderr, "Usage: %s drag <sx> <sy> <ex> <ey> [steps]\n", argv[0]);
        return 1;
    }
    int sx = atoi(argv[2]);
    int sy = atoi(argv[3]);
    int ex = atoi(argv[4]);
    int ey = atoi(argv[5]);
    int steps = (argc >= 7) ? atoi(argv[6]) : 20;
    if (steps < 1) steps = 1;
    if (steps > 100) steps = 100;

    Display *dpy = open_display();
    if (!dpy) return 1;

    /* 1) Move to start position */
    warp_pointer(dpy, sx, sy);
    fake_motion(dpy, sx, sy);
    usleep(30000); /* 30ms */

    /* 2) Mouse down (button 1) */
    fake_button(dpy, 1, 1);
    usleep(50000); /* 50ms hold */

    /* 3) Smooth movement from start to end in N steps.
     *    Each step: XIWarpPointer + XTestFakeMotionEvent.
     *    The XTest motion event generates MotionNotify that applications
     *    (including WebKit/Chromium) use to detect drag movement. */
    for (int i = 1; i <= steps; i++) {
        int ix = sx + (ex - sx) * i / steps;
        int iy = sy + (ey - sy) * i / steps;
        warp_pointer(dpy, ix, iy);
        fake_motion(dpy, ix, iy);
        usleep(10000); /* 10ms between steps */
    }

    /* 4) Mouse up (button 1) */
    fake_button(dpy, 1, 0);

    printf("drag: (%d,%d) -> (%d,%d) steps=%d\n", sx, sy, ex, ey, steps);
    XCloseDisplay(dpy);
    return 0;
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        fprintf(stderr, "Usage:\n  %s move <x> <y>\n  %s drag <sx> <sy> <ex> <ey> [steps]\n",
                argv[0], argv[0]);
        return 1;
    }

    if (strcmp(argv[1], "move") == 0) {
        return cmd_move(argc, argv);
    } else if (strcmp(argv[1], "drag") == 0) {
        return cmd_drag(argc, argv);
    } else {
        /* Legacy: treat as "move <x> <y>" for backward compat */
        if (argc >= 3) {
            int x = atoi(argv[1]);
            int y = atoi(argv[2]);
            /* Rebuild argv to use cmd_move */
            char *new_argv[4] = { argv[0], "move", argv[1], argv[2] };
            return cmd_move(4, new_argv);
        }
        fprintf(stderr, "Unknown command: %s\n", argv[1]);
        return 1;
    }
}
