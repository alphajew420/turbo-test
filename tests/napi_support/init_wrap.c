/* Module init that calls an unsupported entrypoint (napi_wrap). */
#include "napi.h"
napi_value napi_register_module_v1(napi_env env, napi_value exports) {
    napi_value obj;
    napi_create_object(env, &obj);
    napi_wrap(env, obj, (void *)1, NULL, NULL, NULL);
    return exports;
}
