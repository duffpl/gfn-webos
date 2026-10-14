/*
 * Minimal luna-service2 client (GPL-3.0-or-later, ours): one anonymous bus handle served by a
 * GMainLoop on its own thread.
 *
 * Both libraries are dlopen'd so a TV whose bus library differs only loses Bluetooth, not the
 * whole app to a failed load. Every LSCall/LSCallCancel runs on the loop thread (handed over with
 * g_main_context_invoke): the handle is not documented as thread-safe, and serialising there also
 * means a reply can never race the cancel that retires its call.
 *
 * Calls are named by a caller-chosen 64-bit id; replies come back as (id, JSON payload) through
 * the single callback given to gfn_luna_open.
 */
#include <dlfcn.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct LSHandle LSHandle;
typedef struct LSMessage LSMessage;
typedef struct GMainContext GMainContext;
typedef struct GMainLoop GMainLoop;
typedef unsigned long LSMessageToken;
typedef bool (*LSFilterFunc)(LSHandle *sh, LSMessage *reply, void *ctx);
typedef int (*GSourceFunc)(void *data);

/* Layout from luna-service2's lunaservice.h. */
typedef struct {
    int error_code;
    char *message;
    const char *file;
    int line;
    const char *func;
    void *padding;
    unsigned long magic;
} LSError;

typedef void (*gfn_luna_reply_fn)(uint64_t id, const char *payload);

static struct {
    bool (*ErrorInit)(LSError *);
    void (*ErrorFree)(LSError *);
    bool (*Register)(const char *, LSHandle **, LSError *);
    bool (*GmainContextAttach)(LSHandle *, GMainContext *, LSError *);
    bool (*Call)(LSHandle *, const char *, const char *, LSFilterFunc, void *, LSMessageToken *, LSError *);
    bool (*CallCancel)(LSHandle *, LSMessageToken, LSError *);
    const char *(*MessageGetPayload)(LSMessage *);
    GMainContext *(*context_new)(void);
    GMainLoop *(*loop_new)(GMainContext *, int);
    void (*loop_run)(GMainLoop *);
    void (*context_invoke)(GMainContext *, GSourceFunc, void *);
} ls;

static LSHandle *handle;
static GMainContext *context;
static GMainLoop *loop;
static gfn_luna_reply_fn on_reply;

/* Live calls; touched only on the loop thread. */
struct live {
    uint64_t id;
    LSMessageToken token;
    struct live *next;
};
static struct live *calls;

struct job {
    uint64_t id;
    char *uri;     /* NULL: cancel */
    char *payload;
};

static void report_error(uint64_t id, const char *what, LSError *error) {
    char json[512];
    const char *message = error && error->message ? error->message : "unknown error";
    /* Bus messages are plain text without quotes or backslashes in practice; drop any anyway. */
    char clean[384];
    size_t n = 0;
    for (const char *c = message; *c && n + 1 < sizeof clean; c++) {
        if (*c != '"' && *c != '\\' && (unsigned char)*c >= 0x20) clean[n++] = *c;
    }
    clean[n] = 0;
    snprintf(json, sizeof json, "{\"returnValue\":false,\"errorCode\":-1,\"errorText\":\"%s: %s\"}", what, clean);
    on_reply(id, json);
}

static bool reply(LSHandle *sh, LSMessage *message, void *ctx) {
    (void)sh;
    uint64_t id = (uint64_t)(uintptr_t)ctx;
    const char *payload = ls.MessageGetPayload(message);
    on_reply(id, payload ? payload : "{}");
    return true;
}

static int run_job(void *data) {
    struct job *job = data;
    LSError error;
    ls.ErrorInit(&error);
    if (job->uri) {
        LSMessageToken token = 0;
        /* ctx carries the id itself: nothing to free when a call is retired. ids fit uintptr_t
         * because the Rust side counts up from 1 and never reaches 2^32 in one process. */
        if (ls.Call(handle, job->uri, job->payload, reply, (void *)(uintptr_t)job->id, &token, &error)) {
            struct live *call = malloc(sizeof *call);
            if (call) {
                call->id = job->id;
                call->token = token;
                call->next = calls;
                calls = call;
            }
        } else {
            report_error(job->id, "LSCall", &error);
            ls.ErrorFree(&error);
        }
    } else {
        for (struct live **at = &calls; *at; at = &(*at)->next) {
            if ((*at)->id == job->id) {
                struct live *call = *at;
                *at = call->next;
                if (!ls.CallCancel(handle, call->token, &error)) ls.ErrorFree(&error);
                free(call);
                break;
            }
        }
    }
    free(job->uri);
    free(job->payload);
    free(job);
    return 0; /* G_SOURCE_REMOVE */
}

static void *loop_main(void *unused) {
    (void)unused;
    ls.loop_run(loop);
    return NULL;
}

#define LOAD(lib, field, name)                                                  \
    do {                                                                        \
        *(void **)&ls.field = dlsym(lib, name);                                 \
        if (!ls.field) {                                                        \
            snprintf(err, err_len, "%s is missing from the bus library", name); \
            return -1;                                                          \
        }                                                                       \
    } while (0)

/* 0 on success; otherwise -1 with a reason in err. Call once. */
int gfn_luna_open(gfn_luna_reply_fn callback, char *err, size_t err_len) {
    void *glib = dlopen("libglib-2.0.so.0", RTLD_NOW | RTLD_GLOBAL);
    void *lib = dlopen("libluna-service2.so.3", RTLD_NOW | RTLD_GLOBAL);
    if (!glib || !lib) {
        snprintf(err, err_len, "%s", dlerror());
        return -1;
    }
    LOAD(lib, ErrorInit, "LSErrorInit");
    LOAD(lib, ErrorFree, "LSErrorFree");
    LOAD(lib, Register, "LSRegister");
    LOAD(lib, GmainContextAttach, "LSGmainContextAttach");
    LOAD(lib, Call, "LSCall");
    LOAD(lib, CallCancel, "LSCallCancel");
    LOAD(lib, MessageGetPayload, "LSMessageGetPayload");
    LOAD(glib, context_new, "g_main_context_new");
    LOAD(glib, loop_new, "g_main_loop_new");
    LOAD(glib, loop_run, "g_main_loop_run");
    LOAD(glib, context_invoke, "g_main_context_invoke");

    LSError error;
    ls.ErrorInit(&error);
    if (!ls.Register(NULL, &handle, &error)) {
        snprintf(err, err_len, "LSRegister: %s", error.message ? error.message : "failed");
        ls.ErrorFree(&error);
        return -1;
    }
    context = ls.context_new();
    if (!ls.GmainContextAttach(handle, context, &error)) {
        snprintf(err, err_len, "LSGmainContextAttach: %s", error.message ? error.message : "failed");
        ls.ErrorFree(&error);
        return -1;
    }
    loop = ls.loop_new(context, 0);
    on_reply = callback;
    pthread_t thread;
    if (pthread_create(&thread, NULL, loop_main, NULL) != 0) {
        snprintf(err, err_len, "could not start the bus thread");
        return -1;
    }
    pthread_detach(thread);
    return 0;
}

static void submit(uint64_t id, const char *uri, const char *payload) {
    struct job *job = calloc(1, sizeof *job);
    if (!job) return;
    job->id = id;
    job->uri = uri ? strdup(uri) : NULL;
    job->payload = payload ? strdup(payload) : NULL;
    ls.context_invoke(context, run_job, job);
}

/* Replies, including a failed LSCall's error, arrive through the callback until gfn_luna_cancel. */
void gfn_luna_call(uint64_t id, const char *uri, const char *payload) {
    submit(id, uri, payload);
}

void gfn_luna_cancel(uint64_t id) {
    submit(id, NULL, NULL);
}
