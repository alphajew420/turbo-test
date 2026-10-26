/* Module init that calls an unsupported entrypoint (napi_new_instance). */
#include "napi.h"
napi_value napi_register_module_v1(napi_env env, napi_value exports) {
    napi_value ctor, out;
    napi_create_object(env, &ctor);
    napi_new_instance(env, ctor, 0, NULL, &out);
    return exports;
}
