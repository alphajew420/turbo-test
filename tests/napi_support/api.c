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

/* ---- numbers, bigints, dates ----------------------------------------------------------- */

/* numbers() -> [int32s, int64s, statuses of the NULL-result creations] */
static napi_value f_numbers(napi_env env, napi_callback_info info) {
    (void)info;
    static const int32_t I32[] = {0, -1, INT32_MIN, INT32_MAX};
    static const int64_t I64[] = {0, -7, 9007199254740993LL, INT64_MIN, INT64_MAX};
    napi_value a32[4], a64[5];
    for (int i = 0; i < 4; i++) napi_create_int32(env, I32[i], &a32[i]);
    for (int i = 0; i < 5; i++) napi_create_int64(env, I64[i], &a64[i]);
    double st[2] = {napi_create_int32(env, 1, NULL), napi_create_int64(env, 1, NULL)};
    napi_value it[3] = {list(env, 4, a32), list(env, 5, a64), nums(env, 2, st)};
    return list(env, 3, it);
}

/* getInt32(v) -> [status, value (-99 when left alone)] */
static napi_value f_get_int32(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    int32_t n = -99;
    napi_status st = napi_get_value_int32(env, arg(env, &a, 0), &n);
    double r[2] = {st, n};
    return nums(env, 2, r);
}

/* bigints() -> [from int64s, from uint64s, from words, NULL / oversize statuses] */
static napi_value f_bigints(napi_env env, napi_callback_info info) {
    (void)info;
    static const int64_t I[] = {0, -1, INT64_MIN, INT64_MAX};
    static const uint64_t U[] = {0, 1, UINT64_MAX};
    static const uint64_t W[] = {1, 2};
    napi_value ai[4], au[3], aw[4], out;
    for (int i = 0; i < 4; i++) napi_create_bigint_int64(env, I[i], &ai[i]);
    for (int i = 0; i < 3; i++) napi_create_bigint_uint64(env, U[i], &au[i]);
    napi_create_bigint_words(env, 0, 2, W, &aw[0]);
    napi_create_bigint_words(env, 1, 2, W, &aw[1]);
    napi_create_bigint_words(env, 1, 0, W, &aw[2]);
    napi_create_bigint_words(env, 0, 1, W, &aw[3]);
    /* one word past V8's maximum BigInt size: a RangeError, left pending */
    size_t huge = ((size_t)1 << 24) + 1;
    uint64_t *big = calloc(huge, sizeof *big);
    double st[8];
    st[0] = napi_create_bigint_int64(env, 1, NULL);
    st[1] = napi_create_bigint_uint64(env, 1, NULL);
    st[2] = napi_create_bigint_words(env, 0, 1, NULL, &out);
    st[3] = napi_create_bigint_words(env, 0, 1, W, NULL);
    st[4] = napi_create_bigint_words(env, 0, (size_t)INT32_MAX + 1, W, &out);
    st[5] = napi_create_bigint_words(env, 0, huge, big, &out);
    free(big);
    napi_value exc = NULL;
    napi_get_and_clear_last_exception(env, &exc);
    /* with an exception pending, napi_create_bigint_words runs no JS */
    napi_throw_error(env, NULL, "pending");
    st[6] = napi_create_bigint_words(env, 0, 1, W, &out);
    napi_get_and_clear_last_exception(env, &out);
    st[7] = napi_create_bigint_int64(env, 1, &out);
    napi_value it[5] = {list(env, 4, ai), list(env, 3, au), list(env, 4, aw), nums(env, 8, st),
                        or_undef(env, exc)};
    return list(env, 5, it);
}

/* getBigInt64(v) / getBigUint64(v) -> [status, value as a BigInt, lossless] */
static napi_value f_get_bigint64(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    int64_t n = 0;
    bool lossless = false;
    napi_status st = napi_get_value_bigint_int64(env, arg(env, &a, 0), &n, &lossless);
    napi_value v, b;
    napi_create_bigint_int64(env, n, &v);
    napi_get_boolean(env, lossless, &b);
    napi_value it[3] = {num(env, st), v, b};
    return list(env, 3, it);
}
static napi_value f_get_biguint64(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    uint64_t n = 0;
    bool lossless = false;
    napi_status st = napi_get_value_bigint_uint64(env, arg(env, &a, 0), &n, &lossless);
    napi_value v, b;
    napi_create_bigint_uint64(env, n, &v);
    napi_get_boolean(env, lossless, &b);
    napi_value it[3] = {num(env, st), v, b};
    return list(env, 3, it);
}

/* getWords(v, capacity) -> [status, sign, word count, words written (as BigInts)...];
 * capacity -1: a word-count query (NULL sign and words) */
static napi_value f_get_words(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    int cap = (int)argd(env, &a, 1);
    int sign = -1;
    uint64_t words[4] = {0, 0, 0, 0};
    size_t count = cap < 0 ? 0 : (size_t)cap;
    napi_status st = cap < 0 ? napi_get_value_bigint_words(env, arg(env, &a, 0), NULL, &count, NULL)
                             : napi_get_value_bigint_words(env, arg(env, &a, 0), &sign, &count,
                                                           words);
    napi_value it[7];
    it[0] = num(env, st);
    it[1] = num(env, sign);
    it[2] = num(env, (double)count);
    for (int i = 0; i < 4; i++) napi_create_bigint_uint64(env, words[i], &it[3 + i]);
    return list(env, 7, it);
}

/* bigintChecks(big) -> statuses: NULL value / result / lossless / word count, a sign without
 * words, words without a sign, a non-BigInt for each getter */
static napi_value f_bigint_checks(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value big = arg(env, &a, 0), n = num(env, 1);
    int64_t i;
    uint64_t u, w[1];
    bool l;
    int sign;
    size_t count = 1;
    double st[] = {
        napi_get_value_bigint_int64(env, NULL, &i, &l),
        napi_get_value_bigint_int64(env, big, NULL, &l),
        napi_get_value_bigint_int64(env, big, &i, NULL),
        napi_get_value_bigint_uint64(env, NULL, &u, &l),
        napi_get_value_bigint_uint64(env, big, &u, NULL),
        napi_get_value_bigint_words(env, NULL, &sign, &count, w),
        napi_get_value_bigint_words(env, big, &sign, NULL, w),
        napi_get_value_bigint_words(env, big, &sign, &count, NULL),
        napi_get_value_bigint_words(env, big, NULL, &count, w),
        napi_get_value_bigint_int64(env, n, &i, &l),
        napi_get_value_bigint_uint64(env, n, &u, &l),
        napi_get_value_bigint_words(env, n, &sign, &count, w),
        napi_get_value_bigint_words(env, n, NULL, NULL, NULL),
    };
    return nums(env, sizeof st / sizeof st[0], st);
}

/* dates(time, notADate) -> [the date, isDate(date), isDate(notADate), value, statuses] */
static napi_value f_dates(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value date = NULL, other = arg(env, &a, 1);
    napi_status created = napi_create_date(env, argd(env, &a, 0), &date);
    bool is_date = false, other_is = true, b;
    double value = -1, d;
    napi_is_date(env, date, &is_date);
    napi_is_date(env, other, &other_is);
    napi_status got = napi_get_date_value(env, date, &value);
    double st[] = {
        created,
        got,
        napi_get_date_value(env, other, &d),
        napi_get_date_value(env, NULL, &d),
        napi_get_date_value(env, date, NULL),
        napi_is_date(env, NULL, &b),
        napi_is_date(env, date, NULL),
        napi_create_date(env, 0, NULL),
    };
    napi_value bt, bf;
    napi_get_boolean(env, is_date, &bt);
    napi_get_boolean(env, other_is, &bf);
    napi_value it[5] = {or_undef(env, date), bt, bf, num(env, value),
                        nums(env, sizeof st / sizeof st[0], st)};
    return list(env, 5, it);
}

/* ---- strings, symbols, coercions, arrays --------------------------------------------- */

/* strings() -> [latin1 (auto, cut), utf16 (auto, cut), property keys (latin1, utf8, utf16),
 * statuses: a NULL string of length 0 / 1, a NULL result, a length over INT_MAX] */
static napi_value f_strings(napi_env env, napi_callback_info info) {
    (void)info;
    static const uint16_t U16[] = {0x68, 0xe9, 0xd83d, 0xde00, 0};
    napi_value it[8], out;
    napi_create_string_latin1(env, "caf\xe9", NAPI_AUTO_LENGTH, &it[0]);
    napi_create_string_latin1(env, "caf\xe9", 3, &it[1]);
    napi_create_string_utf16(env, U16, NAPI_AUTO_LENGTH, &it[2]);
    napi_create_string_utf16(env, U16, 2, &it[3]);
    node_api_create_property_key_latin1(env, "k\xe9y", NAPI_AUTO_LENGTH, &it[4]);
    node_api_create_property_key_utf8(env, "k\xc3\xa9y", NAPI_AUTO_LENGTH, &it[5]);
    node_api_create_property_key_utf16(env, U16, 4, &it[6]);
    double st[] = {
        napi_create_string_latin1(env, NULL, 0, &out),
        napi_create_string_latin1(env, NULL, 1, &out),
        napi_create_string_latin1(env, "x", 1, NULL),
        napi_create_string_latin1(env, "x", (size_t)INT32_MAX + 1, &out),
        napi_create_string_utf16(env, NULL, NAPI_AUTO_LENGTH, &out),
        napi_create_string_utf16(env, NULL, 0, &out),
        napi_create_string_utf16(env, U16, 1, NULL),
        node_api_create_property_key_latin1(env, NULL, 2, &out),
        node_api_create_property_key_utf8(env, "x", 1, NULL),
        node_api_create_property_key_utf16(env, U16, (size_t)INT32_MAX + 1, &out),
        napi_create_string_utf8(env, NULL, 3, &out),
        napi_create_string_utf8(env, "x", 1, NULL),
    };
    it[7] = nums(env, sizeof st / sizeof st[0], st);
    return list(env, 8, it);
}

/* getLatin1(v, bufsize) / getUtf16(v, bufsize) -> [status, the copy, *result];
 * bufsize -1: a length query (NULL buffer) */
static napi_value f_get_latin1(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    int bufsize = (int)argd(env, &a, 1);
    char buf[16];
    memset(buf, 'Z', sizeof buf);
    size_t res = 999;
    napi_status st = napi_get_value_string_latin1(env, arg(env, &a, 0), bufsize < 0 ? NULL : buf,
                                                  bufsize < 0 ? 0 : (size_t)bufsize, &res);
    napi_value it[3], copy;
    napi_create_string_latin1(env, buf, bufsize > 0 ? res + 1 : 0, &copy);
    it[0] = num(env, st);
    it[1] = copy;
    it[2] = num(env, (double)res);
    return list(env, 3, it);
}
static napi_value f_get_utf16(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    int bufsize = (int)argd(env, &a, 1);
    uint16_t buf[16];
    for (int i = 0; i < 16; i++) buf[i] = 'Z';
    size_t res = 999;
    napi_status st = napi_get_value_string_utf16(env, arg(env, &a, 0), bufsize < 0 ? NULL : buf,
                                                 bufsize < 0 ? 0 : (size_t)bufsize, &res);
    napi_value it[3], copy;
    napi_create_string_utf16(env, buf, bufsize > 0 ? res + 1 : 0, &copy);
    it[0] = num(env, st);
    it[1] = copy;
    it[2] = num(env, (double)res);
    return list(env, 3, it);
}

/* getStringChecks(notAString) -> statuses: NULL value, a non-string, a NULL buffer and result,
 * and a NULL result with a buffer (allowed), for latin1 then utf16 */
static napi_value f_get_string_checks(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value other = arg(env, &a, 0), s = str(env, "abc");
    char b[4];
    uint16_t w[4];
    size_t r;
    double st[] = {
        napi_get_value_string_latin1(env, NULL, b, 4, &r),
        napi_get_value_string_latin1(env, other, b, 4, &r),
        napi_get_value_string_latin1(env, s, NULL, 0, NULL),
        napi_get_value_string_latin1(env, s, b, 4, NULL),
        napi_get_value_string_utf16(env, NULL, w, 4, &r),
        napi_get_value_string_utf16(env, other, w, 4, &r),
        napi_get_value_string_utf16(env, s, NULL, 0, NULL),
        napi_get_value_string_utf16(env, s, w, 4, NULL),
    };
    return nums(env, sizeof st / sizeof st[0], st);
}

static int g_ext_finalized;
static void ext_finalize(napi_env env, void *data, void *hint) {
    (void)env;
    /* the data is the buffer, the hint what was passed */
    if (hint == (void *)&g_ext_finalized) g_ext_finalized++;
    free(data);
}

/* externalStrings() -> [latin1 [status, string, copied], utf16 [...], an external string with
 * no finalizer, statuses of a NULL string / result] */
static napi_value f_external_strings(napi_env env, napi_callback_info info) {
    (void)info;
    char *l1 = malloc(13);
    memcpy(l1, "external \xe9\xe9\xe9", 13);
    uint16_t *u16 = malloc(4 * sizeof *u16);
    u16[0] = 0x20ac;
    u16[1] = 0xd83d;
    u16[2] = 0xde00;
    u16[3] = 0;
    static char fixed[] = "fixed";
    bool c1 = true, c2 = true;
    napi_value s1 = NULL, s2 = NULL, s3 = NULL, out;
    napi_status st1 = node_api_create_external_string_latin1(env, l1, 12, ext_finalize,
                                                             &g_ext_finalized, &s1, &c1);
    napi_status st2 = node_api_create_external_string_utf16(env, u16, NAPI_AUTO_LENGTH,
                                                            ext_finalize, &g_ext_finalized, &s2,
                                                            &c2);
    node_api_create_external_string_latin1(env, fixed, NAPI_AUTO_LENGTH, NULL, NULL, &s3, NULL);
    double st[] = {
        node_api_create_external_string_latin1(env, NULL, 1, NULL, NULL, &out, NULL),
        node_api_create_external_string_utf16(env, u16, 1, NULL, NULL, NULL, NULL),
    };
    napi_value b1, b2, it[4];
    napi_get_boolean(env, c1, &b1);
    napi_get_boolean(env, c2, &b2);
    napi_value r1[3] = {num(env, st1), or_undef(env, s1), b1};
    napi_value r2[3] = {num(env, st2), or_undef(env, s2), b2};
    it[0] = list(env, 3, r1);
    it[1] = list(env, 3, r2);
    it[2] = or_undef(env, s3);
    it[3] = nums(env, 2, st);
    return list(env, 4, it);
}
static napi_value f_ext_finalized(napi_env env, napi_callback_info info) {
    (void)info;
    return num(env, g_ext_finalized);
}

/* symbols(desc | undefined) -> [status, symbol, Symbol.for("tt.key"), statuses] */
static napi_value f_symbols(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value d = arg(env, &a, 0), sym = NULL, sym_for = NULL, out;
    napi_status st = napi_create_symbol(env, is_undefined(env, d) ? NULL : d, &sym);
    node_api_symbol_for(env, "tt.key and more", 6, &sym_for);
    double checks[] = {
        napi_create_symbol(env, NULL, NULL),
        node_api_symbol_for(env, NULL, 2, &out),
        node_api_symbol_for(env, "k", 1, NULL),
    };
    napi_value it[4] = {num(env, st), or_undef(env, sym), or_undef(env, sym_for),
                        nums(env, 3, checks)};
    return list(env, 4, it);
}

/* coerce(v) -> [[status, ToBoolean], [status, ToNumber], what ToNumber left pending,
 * [status, ToObject], what ToObject left pending] (each exception cleared) */
static napi_value f_coerce(napi_env env, napi_callback_info info) {
    Args a = args(env, info);
    napi_value v = arg(env, &a, 0), b = NULL, n = NULL, o = NULL, e1 = NULL, e2 = NULL;
    napi_status sb = napi_coerce_to_bool(env, v, &b);
    napi_status sn = napi_coerce_to_number(env, v, &n);
    napi_get_and_clear_last_exception(env, &e1);
    napi_status so = napi_coerce_to_object(env, v, &o);
    napi_get_and_clear_last_exception(env, &e2);
    napi_value rb[2] = {num(env, sb), or_undef(env, b)};
    napi_value rn[2] = {num(env, sn), or_undef(env, n)};
    napi_value ro[2] = {num(env, so), or_undef(env, o)};
    napi_value it[5] = {list(env, 2, rb), list(env, 2, rn), e1, list(env, 2, ro), e2};
    return list(env, 5, it);
}

/* coerceChecks() -> statuses: NULL value / result for each coercion */
static napi_value f_coerce_checks(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value v = num(env, 1), out;
    double st[] = {
        napi_coerce_to_bool(env, NULL, &out),  napi_coerce_to_bool(env, v, NULL),
        napi_coerce_to_number(env, NULL, &out), napi_coerce_to_number(env, v, NULL),
        napi_coerce_to_object(env, NULL, &out), napi_coerce_to_object(env, v, NULL),
    };
    return nums(env, sizeof st / sizeof st[0], st);
}

/* createArray() -> [the array, status with a NULL result] */
static napi_value f_create_array(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value arr = NULL;
    napi_create_array(env, &arr);
    napi_value it[2] = {or_undef(env, arr), num(env, napi_create_array(env, NULL))};
    return list(env, 2, it);
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
        napi_create_int32(NULL, 1, &out),
        napi_get_value_bigint_words(NULL, v, NULL, NULL, NULL),
        napi_create_bigint_words(NULL, 0, 0, NULL, &out),
        napi_create_date(NULL, 0, &out),
        napi_create_string_latin1(NULL, "x", 1, &out),
        node_api_create_external_string_latin1(NULL, NULL, 0, NULL, NULL, &out, NULL),
        napi_create_symbol(NULL, NULL, &out),
        napi_coerce_to_bool(NULL, v, &out),
        napi_create_array(NULL, &out),
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
    {"numbers", f_numbers},
    {"getInt32", f_get_int32},
    {"bigints", f_bigints},
    {"getBigInt64", f_get_bigint64},
    {"getBigUint64", f_get_biguint64},
    {"getWords", f_get_words},
    {"bigintChecks", f_bigint_checks},
    {"dates", f_dates},
    {"strings", f_strings},
    {"getLatin1", f_get_latin1},
    {"getUtf16", f_get_utf16},
    {"getStringChecks", f_get_string_checks},
    {"externalStrings", f_external_strings},
    {"extFinalized", f_ext_finalized},
    {"symbols", f_symbols},
    {"coerce", f_coerce},
    {"coerceChecks", f_coerce_checks},
    {"createArray", f_create_array},
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
