/* Synthetic native (.node) addon for the segfault-hardening test.
 * Its module init calls napi_new_instance — an N-API entrypoint turbo-test routes to a
 * catchable JS throw ("not implemented") rather than a null-pointer call. Used
 * to prove a misbehaving native addon can no longer SIGSEGV the whole run.
 * Built at test time (see test/compat-napi.test.mjs); the .node is gitignored. */
#include <stddef.h>
typedef int napi_status;
typedef void* napi_value;
typedef void* napi_env;
typedef void* napi_ref;
extern napi_status napi_create_object(napi_env, napi_value*);
extern napi_status napi_new_instance(napi_env, napi_value, size_t, const napi_value*, napi_value*);
napi_value napi_register_module_v1(napi_env env, napi_value exports) {
    napi_value ctor, out;
    napi_create_object(env, &ctor);
    napi_new_instance(env, ctor, 0, NULL, &out);
    return exports;
}
