// CoreML runner for a fixed-shape Nemotron 3 diarization model (.mlmodelc).
//
// Rust owns the streaming state and pads every input to the model's capacity;
// this file only converts between float32 buffers and the model's declared
// multi-array types and strides. Outputs are fp16 with padded row strides on
// some compute units, so they are always read through `strides`.
//
// A handle is used by one thread at a time (Rust holds it behind &mut).
// Each call runs inside its own autorelease pool: the live diarization worker
// is a plain thread without one, and a meeting makes thousands of calls.
#import <CoreML/CoreML.h>
#import <Foundation/Foundation.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    int32_t chunk_frames;     // mel frames in `chunk`
    int32_t spkcache_frames;  // capacity of `spkcache`
    int32_t fifo_frames;      // capacity of `fifo`
    int32_t preds_rows;       // rows of `speaker_preds` (80 ms)
    int32_t hires_rows;       // rows of `speaker_preds_10ms`
    int32_t embs_rows;        // rows of `chunk_pre_encode_embs`
} MeetilyCoreMLShapes;

static const NSInteger kMels = 128;
static const NSInteger kEmb = 512;
static const NSInteger kSpeakers = 8;

static void set_error(char **error, NSString *message) {
    if (error) *error = strdup(message.UTF8String ? message.UTF8String : "CoreML error");
}

static NSArray<NSNumber *> *shape_of(MLModel *model, NSString *name, BOOL input) {
    NSDictionary<NSString *, MLFeatureDescription *> *map = input
        ? model.modelDescription.inputDescriptionsByName
        : model.modelDescription.outputDescriptionsByName;
    return map[name].multiArrayConstraint.shape;
}

static MLMultiArrayDataType type_of(MLModel *model, NSString *name) {
    return model.modelDescription.inputDescriptionsByName[name].multiArrayConstraint.dataType;
}

// Expect [1, rows, cols]; returns rows or -1.
static int32_t rows_of(NSArray<NSNumber *> *shape, NSInteger cols) {
    if (shape.count != 3 || shape[0].integerValue != 1 || shape[2].integerValue != cols) return -1;
    return (int32_t)shape[1].integerValue;
}

@interface MeetilyCoreMLModel : NSObject
@property(nonatomic, strong) MLModel *model;
@property(nonatomic, strong) MLMultiArray *chunk;
@property(nonatomic, strong) MLMultiArray *spkcache;
@property(nonatomic, strong) MLMultiArray *fifo;
@property(nonatomic, strong) MLMultiArray *chunkLength;
@property(nonatomic, strong) MLMultiArray *spkcacheLength;
@property(nonatomic, strong) MLMultiArray *fifoLength;
@property(nonatomic, strong) id<MLFeatureProvider> inputs;
@property(nonatomic) MeetilyCoreMLShapes shapes;
@end
@implementation MeetilyCoreMLModel
@end

// Copy row-major float32 [rows, cols] into a [1, rows, cols] multi-array.
static BOOL write_rows(MLMultiArray *array, const float *src, NSInteger rows, NSInteger cols) {
    NSInteger rowStride = array.strides[1].integerValue, colStride = array.strides[2].integerValue;
    if (array.dataType == MLMultiArrayDataTypeFloat16) {
        _Float16 *dst = (_Float16 *)array.dataPointer;
        for (NSInteger r = 0; r < rows; r++)
            for (NSInteger c = 0; c < cols; c++) dst[r * rowStride + c * colStride] = (_Float16)src[r * cols + c];
        return YES;
    }
    if (array.dataType == MLMultiArrayDataTypeFloat32) {
        float *dst = (float *)array.dataPointer;
        for (NSInteger r = 0; r < rows; r++)
            for (NSInteger c = 0; c < cols; c++) dst[r * rowStride + c * colStride] = src[r * cols + c];
        return YES;
    }
    return NO;
}

// Copy a [1, rows, cols] multi-array (any strides, fp16/fp32) into row-major float32.
static BOOL read_rows(MLMultiArray *array, float *dst, NSInteger rows, NSInteger cols) {
    if (array.shape.count != 3 || array.shape[1].integerValue != rows || array.shape[2].integerValue != cols) return NO;
    NSInteger rowStride = array.strides[1].integerValue, colStride = array.strides[2].integerValue;
    __block BOOL ok = YES;
    [array getBytesWithHandler:^(const void *bytes, NSInteger size) {
        (void)size;
        if (array.dataType == MLMultiArrayDataTypeFloat16) {
            const _Float16 *src = (const _Float16 *)bytes;
            for (NSInteger r = 0; r < rows; r++)
                for (NSInteger c = 0; c < cols; c++) dst[r * cols + c] = (float)src[r * rowStride + c * colStride];
        } else if (array.dataType == MLMultiArrayDataTypeFloat32) {
            const float *src = (const float *)bytes;
            for (NSInteger r = 0; r < rows; r++)
                for (NSInteger c = 0; c < cols; c++) dst[r * cols + c] = src[r * rowStride + c * colStride];
        } else {
            ok = NO;
        }
    }];
    return ok;
}

void *meetily_coreml_load(const char *path, int32_t computeUnits, MeetilyCoreMLShapes *shapes, char **error) {
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
            MeetilyCoreMLShapes s = {
                rows_of(shape_of(model, @"chunk", YES), kMels),
                rows_of(shape_of(model, @"spkcache", YES), kEmb),
                rows_of(shape_of(model, @"fifo", YES), kEmb),
                rows_of(shape_of(model, @"speaker_preds", NO), kSpeakers),
                rows_of(shape_of(model, @"speaker_preds_10ms", NO), kSpeakers),
                rows_of(shape_of(model, @"chunk_pre_encode_embs", NO), kEmb),
            };
            if (s.chunk_frames <= 0 || s.spkcache_frames <= 0 || s.fifo_frames <= 0 || s.preds_rows <= 0 ||
                s.hires_rows <= 0 || s.embs_rows <= 0) {
                set_error(error, @"CoreML model does not have the Nemotron 3 diarization inputs and outputs");
                return NULL;
            }
            MeetilyCoreMLModel *handle = [[MeetilyCoreMLModel alloc] init];
            handle.model = model;
            handle.shapes = s;
            NSError *arrayError = nil;
            handle.chunk = [[MLMultiArray alloc] initWithShape:shape_of(model, @"chunk", YES) dataType:type_of(model, @"chunk") error:&arrayError];
            handle.spkcache = [[MLMultiArray alloc] initWithShape:shape_of(model, @"spkcache", YES) dataType:type_of(model, @"spkcache") error:&arrayError];
            handle.fifo = [[MLMultiArray alloc] initWithShape:shape_of(model, @"fifo", YES) dataType:type_of(model, @"fifo") error:&arrayError];
            handle.chunkLength = [[MLMultiArray alloc] initWithShape:@[@1] dataType:MLMultiArrayDataTypeInt32 error:&arrayError];
            handle.spkcacheLength = [[MLMultiArray alloc] initWithShape:@[@1] dataType:MLMultiArrayDataTypeInt32 error:&arrayError];
            handle.fifoLength = [[MLMultiArray alloc] initWithShape:@[@1] dataType:MLMultiArrayDataTypeInt32 error:&arrayError];
            if (!handle.chunk || !handle.spkcache || !handle.fifo || !handle.chunkLength || !handle.spkcacheLength || !handle.fifoLength) {
                set_error(error, arrayError.localizedDescription ?: @"CoreML input buffers could not be created");
                return NULL;
            }
            handle.inputs = [[MLDictionaryFeatureProvider alloc] initWithDictionary:@{
                @"chunk": [MLFeatureValue featureValueWithMultiArray:handle.chunk],
                @"chunk_lengths": [MLFeatureValue featureValueWithMultiArray:handle.chunkLength],
                @"spkcache": [MLFeatureValue featureValueWithMultiArray:handle.spkcache],
                @"spkcache_lengths": [MLFeatureValue featureValueWithMultiArray:handle.spkcacheLength],
                @"fifo": [MLFeatureValue featureValueWithMultiArray:handle.fifo],
                @"fifo_lengths": [MLFeatureValue featureValueWithMultiArray:handle.fifoLength],
            } error:&arrayError];
            if (!handle.inputs) {
                set_error(error, arrayError.localizedDescription ?: @"CoreML inputs could not be created");
                return NULL;
            }
            if (shapes) *shapes = s;
            return (__bridge_retained void *)handle;
        } @catch (NSException *exception) {
            set_error(error, exception.reason ?: @"CoreML model could not be loaded");
            return NULL;
        }
    }
}

// Inputs are row-major float32 padded to capacity; lengths say how many rows
// are valid. Outputs are row-major float32 of the shapes reported at load.
int32_t meetily_coreml_predict(void *opaque, const float *chunk, int32_t chunkLength, const float *spkcache,
                               int32_t spkcacheLength, const float *fifo, int32_t fifoLength, float *preds,
                               float *hires, float *embs, char **error) {
    @autoreleasepool {
        @try {
            MeetilyCoreMLModel *handle = (__bridge MeetilyCoreMLModel *)opaque;
            MeetilyCoreMLShapes s = handle.shapes;
            if (!write_rows(handle.chunk, chunk, s.chunk_frames, kMels) ||
                !write_rows(handle.spkcache, spkcache, s.spkcache_frames, kEmb) ||
                !write_rows(handle.fifo, fifo, s.fifo_frames, kEmb)) {
                set_error(error, @"Unsupported CoreML input type");
                return 0;
            }
            ((int32_t *)handle.chunkLength.dataPointer)[0] = chunkLength;
            ((int32_t *)handle.spkcacheLength.dataPointer)[0] = spkcacheLength;
            ((int32_t *)handle.fifoLength.dataPointer)[0] = fifoLength;
            NSError *predictError = nil;
            id<MLFeatureProvider> out = [handle.model predictionFromFeatures:handle.inputs error:&predictError];
            if (!out) {
                set_error(error, predictError.localizedDescription ?: @"CoreML prediction failed");
                return 0;
            }
            MLMultiArray *p = [out featureValueForName:@"speaker_preds"].multiArrayValue;
            MLMultiArray *h = [out featureValueForName:@"speaker_preds_10ms"].multiArrayValue;
            MLMultiArray *e = [out featureValueForName:@"chunk_pre_encode_embs"].multiArrayValue;
            if (!p || !h || !e || !read_rows(p, preds, s.preds_rows, kSpeakers) ||
                !read_rows(h, hires, s.hires_rows, kSpeakers) || !read_rows(e, embs, s.embs_rows, kEmb)) {
                set_error(error, @"CoreML outputs have unexpected shapes or types");
                return 0;
            }
            return 1;
        } @catch (NSException *exception) {
            set_error(error, exception.reason ?: @"CoreML prediction failed");
            return 0;
        }
    }
}

void meetily_coreml_free(void *opaque) {
    if (opaque) {
        @autoreleasepool {
            MeetilyCoreMLModel *handle = (__bridge_transfer MeetilyCoreMLModel *)opaque;
            (void)handle;
        }
    }
}

void meetily_coreml_free_string(char *value) { free(value); }
