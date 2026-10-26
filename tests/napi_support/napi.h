/* Minimal Node-API declarations for the turbo-test napi host integration tests.
 * Only the entrypoints src/napi_host.rs exports are declared; every one is left
 * undefined in the compiled .node and resolved against the turbo-test binary at
 * dlopen time (macOS: -undefined dynamic_lookup, Linux: default for -shared). */
#ifndef TT_NAPI_H
#define TT_NAPI_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef int napi_status;
typedef void *napi_value;
typedef void *napi_env;
typedef void *napi_ref;
typedef void *napi_callback_info;
typedef napi_value (*napi_callback)(napi_env, napi_callback_info);
typedef void (*napi_finalize)(napi_env, void *, void *);

#define NAPI_AUTO_LENGTH SIZE_MAX

extern napi_status napi_get_undefined(napi_env, napi_value *);
extern napi_status napi_get_null(napi_env, napi_value *);
extern napi_status napi_get_global(napi_env, napi_value *);
extern napi_status napi_get_boolean(napi_env, bool, napi_value *);
extern napi_status napi_typeof(napi_env, napi_value, int *);
extern napi_status napi_create_uint32(napi_env, uint32_t, napi_value *);
extern napi_status napi_create_double(napi_env, double, napi_value *);
extern napi_status napi_get_value_uint32(napi_env, napi_value, uint32_t *);
extern napi_status napi_create_string_utf8(napi_env, const char *, size_t, napi_value *);
extern napi_status napi_get_value_string_utf8(napi_env, napi_value, char *, size_t, size_t *);
extern napi_status napi_coerce_to_string(napi_env, napi_value, napi_value *);
extern napi_status napi_create_object(napi_env, napi_value *);
extern napi_status napi_create_array_with_length(napi_env, size_t, napi_value *);
extern napi_status napi_set_named_property(napi_env, napi_value, const char *, napi_value);
extern napi_status napi_get_named_property(napi_env, napi_value, const char *, napi_value *);
extern napi_status napi_set_element(napi_env, napi_value, uint32_t, napi_value);
extern napi_status napi_create_function(napi_env, const char *, size_t, napi_callback, void *,
                                        napi_value *);
extern napi_status napi_get_cb_info(napi_env, napi_callback_info, size_t *, napi_value *,
                                    napi_value *, void **);
extern napi_status napi_call_function(napi_env, napi_value, napi_value, size_t,
                                      const napi_value *, napi_value *);
extern napi_status napi_create_arraybuffer(napi_env, size_t, void **, napi_value *);
extern napi_status napi_create_typedarray(napi_env, int, size_t, napi_value, size_t,
                                          napi_value *);
extern napi_status napi_create_external_arraybuffer(napi_env, void *, size_t, napi_finalize,
                                                    void *, napi_value *);
extern napi_status napi_create_buffer(napi_env, size_t, void **, napi_value *);
extern napi_status napi_create_buffer_copy(napi_env, size_t, const void *, void **,
                                           napi_value *);
extern napi_status napi_create_external_buffer(napi_env, size_t, void *, napi_finalize, void *,
                                               napi_value *);
extern napi_status napi_get_buffer_info(napi_env, napi_value, void **, size_t *);
extern napi_status napi_get_value_bool(napi_env, napi_value, bool *);
extern napi_status napi_get_value_double(napi_env, napi_value, double *);
extern napi_status napi_get_value_int64(napi_env, napi_value, int64_t *);
extern napi_status napi_is_array(napi_env, napi_value, bool *);
extern napi_status napi_get_array_length(napi_env, napi_value, uint32_t *);
extern napi_status napi_get_element(napi_env, napi_value, uint32_t, napi_value *);
extern napi_status napi_get_property_names(napi_env, napi_value, napi_value *);
extern napi_status napi_wrap(napi_env, napi_value, void *, void *, void *, napi_ref *);
extern napi_status napi_unwrap(napi_env, napi_value, void **);
extern napi_status napi_new_instance(napi_env, napi_value, size_t, const napi_value *,
                                     napi_value *);
extern napi_status napi_throw(napi_env, napi_value);
extern napi_status napi_throw_error(napi_env, const char *, const char *);
extern napi_status napi_create_error(napi_env, napi_value, napi_value, napi_value *);
extern napi_status napi_is_error(napi_env, napi_value, bool *);
extern napi_status napi_is_exception_pending(napi_env, bool *);
extern napi_status napi_get_and_clear_last_exception(napi_env, napi_value *);
extern napi_status napi_create_reference(napi_env, napi_value, uint32_t, napi_ref *);
extern napi_status napi_get_reference_value(napi_env, napi_ref, napi_value *);
extern napi_status napi_delete_reference(napi_env, napi_ref);
extern napi_status napi_reference_unref(napi_env, napi_ref, uint32_t *);
extern napi_status napi_add_env_cleanup_hook(napi_env, void *, void *);
extern napi_status napi_stub(void);
extern napi_status napi_define_class(napi_env, const char *, size_t, napi_callback, void *,
                                     size_t, const void *, napi_value *);
extern napi_status napi_create_threadsafe_function(napi_env, napi_value, napi_value, napi_value,
                                                   size_t, size_t, void *, void *, void *,
                                                   void *, void **);
extern napi_status napi_call_threadsafe_function(void *, void *, int);
extern napi_status napi_unref_threadsafe_function(napi_env, void *);

#endif
