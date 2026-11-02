// General CoreML runner: named multi-array inputs and outputs of any rank.
//
// Rust passes row-major float32/int32 buffers; each input is converted to the
// type the model declares (fp16/fp32/int32), and each output is read through
// its strides (fp16 outputs often have padded rows) back to row-major f32/i32.
// One thread uses a handle at a time; every call has its own autorelease pool
// because callers include plain worker threads without one.
#import <CoreML/CoreML.h>
#import <Foundation/Foundation.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum { MEETILY_F32 = 0, MEETILY_I32 = 1 };

typedef struct {
    const char *name;
    int32_t dtype;
    int32_t rank;
    const int64_t *shape;
    const void *data;  // row-major, dtype elements
} MeetilyMlInput;

typedef struct {
    const char *name;
    int32_t dtype;     // requested element type of `data`
    int64_t capacity;  // elements available in `data`
    void *data;
    int32_t rank;      // filled in
    int64_t shape[8];  // filled in
} MeetilyMlOutput;

static void set_error(char **error, NSString *message) {
    if (error) *error = strdup(message.UTF8String ? message.UTF8String : "CoreML error");
}

// Element kinds on either side of a copy.
enum { K_F16, K_F32, K_I32 };

static int kind_of(MLMultiArrayDataType type) {
    switch (type) {
        case MLMultiArrayDataTypeFloat16: return K_F16;
        case MLMultiArrayDataTypeFloat32: return K_F32;
        case MLMultiArrayDataTypeInt32: return K_I32;
        default: return -1;
    }
}

static inline double load_value(const void *base, int kind, int64_t i) {
    switch (kind) {
        case K_F16: return ((const _Float16 *)base)[i];
        case K_F32: return ((const float *)base)[i];
        default: return ((const int32_t *)base)[i];
    }
}

static inline void store_value(void *base, int kind, int64_t i, double v) {
    switch (kind) {
        case K_F16: ((_Float16 *)base)[i] = (_Float16)v; break;
        case K_F32: ((float *)base)[i] = (float)v; break;
        default: ((int32_t *)base)[i] = (int32_t)v; break;
    }
}

// Copy between a row-major buffer and a strided multi-array of the same shape.
// `to_array` chooses the direction. Shape and strides are read into C arrays
// once; the strided offset is updated incrementally (no per-element messages).
static void copy_strided(MLMultiArray *array, int array_kind, void *rowmajor, int rowmajor_kind, BOOL to_array,
                         const void *array_bytes) {
    NSInteger rank = (NSInteger)array.shape.count;
    int64_t shape[8], strides[8], index[8] = {0}, count = 1;
    BOOL contiguous = YES;
    int64_t expected = 1;
    for (NSInteger d = rank - 1; d >= 0; d--) {
        shape[d] = array.shape[d].longLongValue;
        strides[d] = array.strides[d].longLongValue;
        if (shape[d] > 1 && strides[d] != expected) contiguous = NO;
        expected *= shape[d];
        count *= shape[d];
    }
    void *dst_array = to_array ? array.dataPointer : NULL;
    if (contiguous && array_kind == rowmajor_kind) {
        size_t size = array_kind == K_F16 ? 2 : 4;
        if (to_array) memcpy(dst_array, rowmajor, (size_t)count * size);
        else memcpy(rowmajor, array_bytes, (size_t)count * size);
        return;
    }
    int64_t physical = 0;
    for (int64_t logical = 0; logical < count; logical++) {
        if (contiguous) physical = logical;
        if (to_array) store_value(dst_array, array_kind, physical, load_value(rowmajor, rowmajor_kind, logical));
        else store_value(rowmajor, rowmajor_kind, logical, load_value(array_bytes, array_kind, physical));
        if (contiguous) continue;
        for (NSInteger d = rank - 1; d >= 0; d--) {
            physical += strides[d];
            if (++index[d] < shape[d]) break;
            physical -= strides[d] * shape[d];
            index[d] = 0;
        }
    }
}

void *meetily_ml_load(const char *path, int32_t computeUnits, char **error) {
    @autoreleasepool {
        @try {
            NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path] isDirectory:YES];
            MLModelConfiguration *config = [[MLModelConfiguration alloc] init];
            config.computeUnits = (MLComputeUnits)computeUnits;
            NSError *loadError = nil;
            MLModel *model = [MLModel modelWithContentsOfURL:url configuration:config error:&loadError];
            if (!model) {
                set_error(error, loadError.localizedDescription ?: @"CoreML model could not be loaded");
                return NULL;
            }
            return (__bridge_retained void *)model;
        } @catch (NSException *exception) {
            set_error(error, exception.reason ?: @"CoreML model could not be loaded");
            return NULL;
        }
    }
}

int32_t meetily_ml_predict(void *opaque, const MeetilyMlInput *inputs, int32_t inputCount,
                           MeetilyMlOutput *outputs, int32_t outputCount, char **error) {
    @autoreleasepool {
        @try {
            MLModel *model = (__bridge MLModel *)opaque;
            NSDictionary<NSString *, MLFeatureDescription *> *declared = model.modelDescription.inputDescriptionsByName;
            NSMutableDictionary<NSString *, MLFeatureValue *> *features = [NSMutableDictionary dictionary];
            for (int32_t i = 0; i < inputCount; i++) {
                const MeetilyMlInput *input = &inputs[i];
                NSString *name = [NSString stringWithUTF8String:input->name];
                MLFeatureDescription *description = declared[name];
                if (!description.multiArrayConstraint) {
                    set_error(error, [NSString stringWithFormat:@"CoreML model has no multi-array input %@", name]);
                    return 0;
                }
                NSMutableArray<NSNumber *> *shape = [NSMutableArray array];
                for (int32_t d = 0; d < input->rank; d++) [shape addObject:@(input->shape[d])];
                NSError *arrayError = nil;
                MLMultiArrayDataType type = description.multiArrayConstraint.dataType;
                MLMultiArray *array = [[MLMultiArray alloc] initWithShape:shape dataType:type error:&arrayError];
                if (!array) {
                    set_error(error, arrayError.localizedDescription ?: @"CoreML input could not be created");
                    return 0;
                }
                int arrayKind = kind_of(type);
                if (arrayKind >= 0) {
                    copy_strided(array, arrayKind, (void *)input->data, input->dtype == MEETILY_F32 ? K_F32 : K_I32, YES, NULL);
                }
                if (arrayKind < 0) {
                    set_error(error, [NSString stringWithFormat:@"Unsupported CoreML input type for %@", name]);
                    return 0;
                }
                features[name] = [MLFeatureValue featureValueWithMultiArray:array];
            }
            NSError *providerError = nil;
            MLDictionaryFeatureProvider *provider = [[MLDictionaryFeatureProvider alloc] initWithDictionary:features error:&providerError];
            if (!provider) {
                set_error(error, providerError.localizedDescription ?: @"CoreML inputs could not be created");
                return 0;
            }
            NSError *predictError = nil;
            id<MLFeatureProvider> result = [model predictionFromFeatures:provider error:&predictError];
            if (!result) {
                set_error(error, predictError.localizedDescription ?: @"CoreML prediction failed");
                return 0;
            }
            for (int32_t i = 0; i < outputCount; i++) {
                MeetilyMlOutput *output = &outputs[i];
                NSString *name = [NSString stringWithUTF8String:output->name];
                MLMultiArray *array = [result featureValueForName:name].multiArrayValue;
                if (!array || array.shape.count > 8 || (int64_t)array.count > output->capacity) {
                    set_error(error, [NSString stringWithFormat:@"CoreML output %@ is missing or larger than expected", name]);
                    return 0;
                }
                output->rank = (int32_t)array.shape.count;
                for (NSUInteger d = 0; d < array.shape.count; d++) output->shape[d] = array.shape[d].longLongValue;
                int arrayKind = kind_of(array.dataType);
                int wantKind = output->dtype == MEETILY_F32 ? K_F32 : K_I32;
                if (arrayKind >= 0) {
                    [array getBytesWithHandler:^(const void *bytes, NSInteger size) {
                        (void)size;
                        copy_strided(array, arrayKind, output->data, wantKind, NO, bytes);
                    }];
                }
                if (arrayKind < 0) {
                    set_error(error, [NSString stringWithFormat:@"Unsupported CoreML output type for %@", name]);
                    return 0;
                }
            }
            return 1;
        } @catch (NSException *exception) {
            set_error(error, exception.reason ?: @"CoreML prediction failed");
            return 0;
        }
    }
}

void meetily_ml_free(void *opaque) {
    if (opaque) {
        @autoreleasepool {
            MLModel *model = (__bridge_transfer MLModel *)opaque;
            (void)model;
        }
    }
}

void meetily_ml_free_string(char *value) { free(value); }
