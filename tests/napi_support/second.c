/* A second addon, loaded next to full.node: its own function table (who) must not be
 * confused with the first addon's. */
#include "napi.h"

static napi_value f_who(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value v;
    napi_create_string_utf8(env, "second", NAPI_AUTO_LENGTH, &v);
    return v;
}

napi_value napi_register_module_v1(napi_env env, napi_value exports) {
    napi_value f;
    napi_create_function(env, "who", NAPI_AUTO_LENGTH, f_who, NULL, &f);
    napi_set_named_property(env, exports, "who", f);
    return exports;
}
