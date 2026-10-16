/*
 * jack2pulse — tiny JACK → PulseAudio bridge for Android/Termux.
 *
 * Termux's jack2 ships an OpenSL ES backend that keeps only one buffer
 * queued and re-enqueues after the previous one *finished*, which crackles
 * at every period size (tested 240…4096 frames on a Pixel 7). PulseAudio's
 * module-sles-sink is clean. So: run jackd on its software-timed `dummy`
 * backend purely as scsynth's clock, and pipe JACK's audio into PulseAudio:
 *
 *     jackd -d dummy -r 44100 -p 512 -w 11610 &
 *     mkfifo f; pacat --raw --format=s16le --rate=44100 --channels=2 \
 *                     --latency-msec=40 f &  jack2pulse > f
 *
 * The bridge registers two input ports, auto-connects them to the ports
 * matching the given prefix (default "SuperCollider:out_") whenever they
 * appear, converts to interleaved s16le and writes to stdout through a
 * lock-free ring buffer.
 *
 * Latency control: the pipe is shrunk to one page (F_SETPIPE_SZ) so the
 * kernel cannot hoard audio, and the ring buffer's average fill is steered
 * towards `target` frames by dropping or duplicating ONE frame per 1024
 * whenever the average sits outside [target/2, target*2]. That is ~0.1 %
 * rate trimming — inaudible — and it also absorbs the long-term clock
 * drift between the dummy backend and the OpenSL sink. Hard overflows
 * (sink stalled) are counted on stderr.
 *
 * Build (Termux):  clang -O2 -o $PREFIX/bin/jack2pulse jack2pulse.c -ljack
 * Usage:           jack2pulse [source-port-prefix] [target-frames]
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <jack/jack.h>
#include <jack/ringbuffer.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_FRAMES 16384
#define RB_SECONDS 1

static jack_client_t *client;
static jack_port_t *in_port[2];
static jack_ringbuffer_t *rb;
static volatile sig_atomic_t running = 1;
static volatile unsigned long overflows = 0;
static int16_t scratch[MAX_FRAMES * 2];

static void on_signal(int sig) { (void)sig; running = 0; }

static inline int16_t to_s16(float v) {
    if (v > 1.0f) v = 1.0f;
    if (v < -1.0f) v = -1.0f;
    return (int16_t)(v * 32767.0f);
}

static int process(jack_nframes_t nframes, void *arg) {
    (void)arg;
    if (nframes > MAX_FRAMES) nframes = MAX_FRAMES;
    const float *l = jack_port_get_buffer(in_port[0], nframes);
    const float *r = jack_port_get_buffer(in_port[1], nframes);
    for (jack_nframes_t i = 0; i < nframes; i++) {
        scratch[2 * i] = to_s16(l[i]);
        scratch[2 * i + 1] = to_s16(r[i]);
    }
    size_t bytes = (size_t)nframes * 4;
    if (jack_ringbuffer_write_space(rb) >= bytes) {
        jack_ringbuffer_write(rb, (const char *)scratch, bytes);
    } else {
        overflows++;
    }
    return 0;
}

static void try_connect(const char *prefix) {
    for (int i = 0; i < 2; i++) {
        if (jack_port_connected(in_port[i])) continue;
        char src[128];
        snprintf(src, sizeof src, "%s%d", prefix, i + 1);
        if (jack_port_by_name(client, src) != NULL) {
            if (jack_connect(client, src, jack_port_name(in_port[i])) == 0)
                fprintf(stderr, "jack2pulse: connected %s -> %s\n", src, jack_port_name(in_port[i]));
        }
    }
}

static int write_all(const char *buf, size_t n) {
    size_t off = 0;
    while (off < n && running) {
        ssize_t w = write(STDOUT_FILENO, buf + off, n - off);
        if (w < 0) return -1;
        off += (size_t)w;
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *prefix = argc > 1 ? argv[1] : "SuperCollider:out_";
    long target = argc > 2 ? atol(argv[2]) : 0;   /* frames; 0 = one period */

    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    signal(SIGPIPE, on_signal);

    jack_status_t status;
    client = jack_client_open("jack2pulse", JackNoStartServer, &status);
    if (!client) {
        fprintf(stderr, "jack2pulse: cannot connect to jackd (status 0x%x)\n", status);
        return 1;
    }
    jack_nframes_t rate = jack_get_sample_rate(client);
    jack_nframes_t period = jack_get_buffer_size(client);
    if (target <= 0) target = period;
    size_t rb_bytes = (size_t)rate * 4 * RB_SECONDS;
    rb = jack_ringbuffer_create(rb_bytes);
    jack_ringbuffer_mlock(rb);

#ifdef F_SETPIPE_SZ
    /* Keep the kernel pipe tiny so it cannot add hundreds of ms of latency. */
    if (fcntl(STDOUT_FILENO, F_SETPIPE_SZ, 4096) < 0)
        fprintf(stderr, "jack2pulse: F_SETPIPE_SZ failed (stdout not a pipe?)\n");
#endif

    in_port[0] = jack_port_register(client, "in_1", JACK_DEFAULT_AUDIO_TYPE, JackPortIsInput, 0);
    in_port[1] = jack_port_register(client, "in_2", JACK_DEFAULT_AUDIO_TYPE, JackPortIsInput, 0);
    jack_set_process_callback(client, process, NULL);
    if (jack_activate(client)) {
        fprintf(stderr, "jack2pulse: cannot activate client\n");
        return 1;
    }
    fprintf(stderr, "jack2pulse: rate=%u period=%u target=%ld frames, waiting for %s1/2\n",
            rate, period, target, prefix);

    useconds_t quarter_period_us = (useconds_t)(250000.0 * period / rate);
    char *out = malloc(rb_bytes);
    unsigned tick = 0;
    unsigned long last_over = 0;
    double avg_fill = (double)target;     /* frames, exponential average */
    long trim = 0;                        /* +1 drop a frame, -1 duplicate, 0 none */
    unsigned long frames_since_trim = 0;
    unsigned long dropped = 0, padded = 0;

    while (running) {
        if ((tick++ % 100) == 0) try_connect(prefix);
        size_t avail = jack_ringbuffer_read_space(rb);
        if (avail < 4) {
            usleep(quarter_period_us);
            continue;
        }
        size_t frames = avail / 4;
        avg_fill += 0.02 * ((double)frames - avg_fill);
        if (avg_fill > 2.0 * target) trim = 1;
        else if (avg_fill < 0.5 * target) trim = -1;
        else trim = 0;

        size_t n = jack_ringbuffer_read(rb, out, frames * 4);
        frames = n / 4;
        if (trim == 1 && frames >= 2) {
            /* Drop one frame per 1024 played: remove the last frame of this chunk. */
            frames_since_trim += frames;
            if (frames_since_trim >= 1024) { frames_since_trim = 0; n -= 4; dropped++; }
            if (write_all(out, n) < 0) break;
        } else if (trim == -1) {
            frames_since_trim += frames;
            if (write_all(out, n) < 0) break;
            if (frames_since_trim >= 1024) {
                frames_since_trim = 0; padded++;
                if (write_all(out + n - 4, 4) < 0) break;   /* duplicate last frame */
            }
        } else {
            frames_since_trim = 0;
            if (write_all(out, n) < 0) break;
        }

        if (overflows != last_over) {
            fprintf(stderr, "jack2pulse: ring buffer overflow (%lu total) — sink stalled\n", overflows);
            last_over = overflows;
        }
    }
    jack_client_close(client);
    fprintf(stderr, "jack2pulse: exit (overflows=%lu dropped=%lu padded=%lu avg_fill=%.0f)\n",
            overflows, dropped, padded, avg_fill);
    return 0;
}
