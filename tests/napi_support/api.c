/* The Node-API surface beyond full.c, one section per family (api.test.ts asserts each against
 * the values Node 26 produces with the same fixture). Every probe hands its napi statuses and
 * results back to JS in an array. */
#include "napi.h"
#include <stdlib.h>
#include <string.h>

#define MAXA 8

typedef struct {
    size_t argc;
    napi_value argv[MAXA];
    napi_value self;
} Args;

static napi_value undef(napi_env env) {
    napi_value v;
    napi_get_undefined(env, &v);
    return v;
}
static napi_value num(napi_env env, double d) {
    napi_value v;
    napi_create_double(env, d, &v);
    return v;
}
static napi_value str(napi_env env, const char *s) {
    napi_value v;
    if (s == NULL) return undef(env);
    napi_create_string_utf8(env, s, NAPI_AUTO_LENGTH, &v);
    return v;
}
static napi_value or_undef(napi_env env, napi_value v) { return v ? v : undef(env); }
static Args args(napi_env env, napi_callback_info info) {
    Args a;
    a.argc = MAXA;
    napi_get_cb_info(env, info, &a.argc, a.argv, &a.self, NULL);
    return a;
}
static napi_value arg(napi_env env, Args *a, size_t i) {
    return i < a->argc ? a->argv[i] : undef(env);
}
static double argd(napi_env env, Args *a, size_t i) {
    double d = 0;
    napi_get_value_double(env, arg(env, a, i), &d);
    return d;
}
static bool is_undefined(napi_env env, napi_value v) {
    int t = -1;
    napi_typeof(env, v, &t);
    return t == 0;
}
/* malloc'd UTF-8 copy of a JS string argument, NULL for undefined */
static char *cstr(napi_env env, napi_value v) {
    if (is_undefined(env, v)) return NULL;
    size_t len = 0;
    napi_get_value_string_utf8(env, v, NULL, 0, &len);
    char *b = malloc(len + 1);
    napi_get_value_string_utf8(env, v, b, len + 1, &len);
    return b;
}
static napi_value list(napi_env env, size_t n, const napi_value *items) {
    napi_value arr;
    napi_create_array_with_length(env, n, &arr);
    for (size_t i = 0; i < n; i++) napi_set_element(env, arr, (uint32_t)i, items[i]);
    return arr;
}
static napi_value nums(napi_env env, size_t n, const double *items) {
    napi_value arr;
    napi_create_array_with_length(env, n, &arr);
    for (size_t i = 0; i < n; i++) napi_set_element(env, arr, (uint32_t)i, num(env, items[i]));
    return arr;
}
/* The last call's error info, copied before any other napi call (each one resets it). */
typedef struct {
    napi_status status, code;
    const char *message;
    uint32_t engine;
} LastError;
static LastError snap(napi_env env) {
    const napi_extended_error_info *info = NULL;
    LastError e = {napi_get_last_error_info(env, &info), 0, NULL, 0};
    if (e.status == napi_ok) {
        e.code = info->error_code;
        e.message = info->error_message;
        e.engine = info->engine_error_code;
    }
    return e;
}
/* [error_code, error_message (undefined for NULL), engine_error_code] */
static napi_value err_value(napi_env env, LastError e) {
    if (e.status != napi_ok) return num(env, -e.status);
    napi_value it[3] = {num(env, e.code), str(env, e.message), num(env, e.engine)};
    return list(env, 3, it);
}
/* ---- errors --------------------------------------------------------------------------- */

typedef napi_status (*ThrowFn)(napi_env, const char *, const char *);
typedef napi_status (*CreateFn)(napi_env, napi_value, napi_value, napi_value *);
static const ThrowFn THROWS[] = {napi_throw_error, napi_throw_type_error, napi_throw_range_error,
                                 node_api_throw_syntax_error};
static const CreateFn CREATES[] = {napi_create_error, napi_create_type_error,
                                   napi_create_range_error, node_api_create_syntax_error};

/* throwKind(kind, code | undefined, msg): napi_throw_*error; the error reaches the caller */
static napi_value f_throw_kind(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    char *code = cstr(env, arg(env, &a, 1)), *msg = cstr(env, arg(env, &a, 2));
    THROWS[(int)argd(env, &a, 0)](env, code, msg);
    free(code);
    free(msg);
    return NULL;
}

/* createKind(kind, code, msg) -> [status, error]: napi_create_*error (code undefined: NULL) */
static napi_value f_create_kind(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value code = arg(env, &a, 1), out = NULL;
    napi_status st = CREATES[(int)argd(env, &a, 0)](env, is_undefined(env, code) ? NULL : code,
                                                    arg(env, &a, 2), &out);
    napi_value it[2] = {num(env, st), or_undef(env, out)};
    return list(env, 2, it);
}

/* errorChecks() -> statuses: each throw with a NULL message, each create with a NULL message
 * and with a NULL result, then the last error after the last of them */
static napi_value f_error_checks(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value msg = str(env, "m"), out;
    double st[12];
    for (int k = 0; k < 4; k++) {
        st[k] = THROWS[k](env, "CODE", NULL);
        st[4 + k] = CREATES[k](env, NULL, NULL, &out);
        st[8 + k] = CREATES[k](env, NULL, msg, NULL);
    }
    LastError last = snap(env);
    napi_value it[2] = {nums(env, 12, st), err_value(env, last)};
    return list(env, 2, it);
}

/* lastErrorInfo(notANumber) -> the last error after: a failing call, then again (reading it
 * changes nothing), a successful call, a call of napi_get_last_error_info with a NULL result
 * (its status), and the last error after that call */
static napi_value f_last_error_info(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value bad = arg(env, &a, 0), one = num(env, 1);
    double d;
    napi_get_value_double(env, bad, &d);
    LastError e0 = snap(env), e1 = snap(env);
    napi_get_value_double(env, one, &d);
    LastError e2 = snap(env);
    napi_status null_result = napi_get_last_error_info(env, NULL);
    LastError e3 = snap(env);
    napi_value it[5] = {err_value(env, e0), err_value(env, e1), err_value(env, e2),
                        num(env, null_result), err_value(env, e3)};
    return list(env, 5, it);
}

/* fatalException(err) -> status of napi_fatal_exception (err undefined: NULL) */
static napi_value f_fatal_exception(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value err = arg(env, &a, 0);
    return num(env, napi_fatal_exception(env, is_undefined(env, err) ? NULL : err));
}

/* ---- every entrypoint with a NULL env is napi_invalid_arg ------------------------------- */

static napi_value f_null_env(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value v = num(env, 1), out;
    const napi_extended_error_info *ei;
    double d;
    double st[] = {
        napi_get_undefined(NULL, &out),
        napi_get_value_double(NULL, v, &d),
        napi_coerce_to_string(NULL, v, &out),
        napi_create_error(NULL, NULL, v, &out),
        napi_throw_type_error(NULL, NULL, "m"),
        napi_get_last_error_info(NULL, &ei),
        napi_fatal_exception(NULL, v),
    };
    return nums(env, sizeof st / sizeof st[0], st);
}

static const struct {
    const char *name;
    napi_callback cb;
} FNS[] = {
    {"throwKind", f_throw_kind},
    {"createKind", f_create_kind},
    {"errorChecks", f_error_checks},
    {"lastErrorInfo", f_last_error_info},
    {"fatalException", f_fatal_exception},
    {"nullEnv", f_null_env},
};

napi_value napi_register_module_v1(napi_env env, napi_value exports) {
    for (size_t i = 0; i < sizeof FNS / sizeof FNS[0]; i++) {
        napi_value f;
        napi_create_function(env, FNS[i].name, NAPI_AUTO_LENGTH, FNS[i].cb, NULL, &f);
        napi_set_named_property(env, exports, FNS[i].name, f);
    }
    return exports;
}
