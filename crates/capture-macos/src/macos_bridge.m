#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <AudioToolbox/AudioToolbox.h>
#import <AVFoundation/AVFoundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreMedia/CoreMedia.h>
#import <AppKit/AppKit.h>
#include <mach/mach_time.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    uint32_t object_id;
    uint32_t pid;
    uint32_t running_output;
    char bundle_id[256];
} CspaMacProcessObject;

typedef struct {
    uint32_t object_id;
    uint32_t is_default;
    uint32_t nominal_rate_hz;
    uint32_t channels;
    char uid[256];
    char name[256];
} CspaMacOutputDevice;

typedef struct {
    uint32_t object_id;
    uint32_t is_default;
    uint32_t nominal_rate_hz;
    uint32_t channels;
    uint32_t transport_type;
    char uid[256];
    char name[256];
} CspaMacInputDevice;

typedef struct {
    uint32_t pid;
    char bundle_id[256];
    char app_name[256];
} CspaMacSckApp;

typedef struct {
    uint32_t pid;
    char bundle_id[256];
    char app_name[256];
    char exe_path[1024];
} CspaMacRunningApp;

typedef void (*CspaAudioPacketCallback)(
    void *user_data,
    uint32_t num_buffers,
    const float *const *buffer_ptrs,
    const uint32_t *buffer_channels,
    uint32_t frames,
    uint64_t timestamp_ns,
    uint32_t timestamp_valid
);

static OSStatus cspa_get_prop(AudioObjectID obj, AudioObjectPropertySelector sel,
                              AudioObjectPropertyScope scope, UInt32 *size, void *data) {
    AudioObjectPropertyAddress addr = {sel, scope, kAudioObjectPropertyElementMain};
    return AudioObjectGetPropertyData(obj, &addr, 0, NULL, size, data);
}

static uint64_t cspa_host_ticks_to_ns(uint64_t ticks) {
    static mach_timebase_info_data_t info = {0, 0};
    if (info.denom == 0) {
        mach_timebase_info(&info);
    }
    if (info.denom == 0) return ticks;
    __uint128_t scaled = ((__uint128_t)ticks * (__uint128_t)info.numer) / (__uint128_t)info.denom;
    return (uint64_t)scaled;
}

void cspa_mac_os_version(uint32_t *major_out, uint32_t *minor_out, uint32_t *patch_out) {
    @autoreleasepool {
        NSOperatingSystemVersion ver = NSProcessInfo.processInfo.operatingSystemVersion;
        if (major_out) *major_out = (uint32_t)ver.majorVersion;
        if (minor_out) *minor_out = (uint32_t)ver.minorVersion;
        if (patch_out) *patch_out = (uint32_t)ver.patchVersion;
    }
}

int cspa_mac_sck_preflight(void) {
    return CGPreflightScreenCaptureAccess() ? 1 : 0;
}

int cspa_mac_microphone_auth_status(void) {
    @autoreleasepool {
        return (int)[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    }
}

static bool cspa_wait_sema(dispatch_semaphore_t signal, double seconds) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:seconds];
    do {
        if (dispatch_semaphore_wait(signal, DISPATCH_TIME_NOW) == 0) return true;
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.005, false);
    } while ([deadline timeIntervalSinceNow] > 0);
    return false;
}

int cspa_mac_is_interactive_gui_session(void) {
    const char *ssh_conn = getenv("SSH_CONNECTION");
    const char *ssh_tty = getenv("SSH_TTY");
    const char *no_prompt = getenv("CSPA_DISABLE_TCC_PROMPT");
    if ((ssh_conn && ssh_conn[0] != '\0') ||
        (ssh_tty && ssh_tty[0] != '\0') ||
        (no_prompt && no_prompt[0] != '\0')) {
        return 0;
    }
    CFDictionaryRef session = CGSessionCopyCurrentDictionary();
    if (!session) {
        return 0;
    }
    int on_console = 1;
    CFBooleanRef console_val = (CFBooleanRef)CFDictionaryGetValue(session, kCGSessionOnConsoleKey);
    if (console_val && CFGetTypeID(console_val) == CFBooleanGetTypeID()) {
        on_console = CFBooleanGetValue(console_val) ? 1 : 0;
    }
    CFRelease(session);
    return on_console;
}

int cspa_mac_bundle_missing_usage_description(const char *plist_key) {
    @autoreleasepool {
        if (!plist_key || plist_key[0] == '\0') return 0;
        NSBundle *bundle = [NSBundle mainBundle];
        if (!bundle) return 0;
        NSString *bundle_path = bundle.bundlePath;
        NSString *bundle_id = bundle.bundleIdentifier;
        if (!bundle_id || bundle_id.length == 0 || !bundle_path || ![bundle_path hasSuffix:@".app"]) {
            return 0;
        }
        NSString *key_str = [NSString stringWithUTF8String:plist_key];
        if (!key_str) return 0;
        id val = [bundle objectForInfoDictionaryKey:key_str];
        if (!val) return 1;
        if ([val isKindOfClass:[NSString class]] && ((NSString *)val).length == 0) {
            return 1;
        }
        return 0;
    }
}

int cspa_mac_request_microphone_access(double timeout_sec) {
    @autoreleasepool {
        if (cspa_mac_bundle_missing_usage_description("NSMicrophoneUsageDescription")) {
            return 1;
        }
        AVAuthorizationStatus cur = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
        if (cur != AVAuthorizationStatusNotDetermined) {
            return (int)cur;
        }
        if (!cspa_mac_is_interactive_gui_session()) {
            return 0;
        }
        double wait_s = (timeout_sec > 0.05 && timeout_sec <= 120.0) ? timeout_sec : 10.0;
        dispatch_semaphore_t sema = dispatch_semaphore_create(0);
        __block BOOL granted_val = NO;
        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) {
            granted_val = granted;
            dispatch_semaphore_signal(sema);
        }];
        if (!cspa_wait_sema(sema, wait_s)) {
            return (int)[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
        }
        return granted_val ? 3 : 2;
    }
}

int cspa_mac_sck_request_access(void) {
    @autoreleasepool {
        if (CGPreflightScreenCaptureAccess()) {
            return 1;
        }
        if (!cspa_mac_is_interactive_gui_session()) {
            return 0;
        }
        return CGRequestScreenCaptureAccess() ? 1 : 0;
    }
}

static bool cspa_is_tcc_denied_osstatus(OSStatus st) {
    /* 'wat?' (0x7761743F = 2003332927) and '!hog' (kAudioDevicePermissionsError = 0x21686F67 = 560492391) */
    return st == (OSStatus)0x7761743F || st == (OSStatus)0x21686F67;
}

int cspa_mac_system_audio_tap_preflight(void) {
    @autoreleasepool {
        if (cspa_mac_bundle_missing_usage_description("NSAudioCaptureUsageDescription")) {
            return -2;
        }
        if (@available(macOS 14.2, *)) {
            AudioObjectID objs[64];
            UInt32 size = sizeof(objs);
            OSStatus st = cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyProcessObjectList,
                                        kAudioObjectPropertyScopeGlobal, &size, objs);
            if (cspa_is_tcc_denied_osstatus(st)) {
                return 2;
            }
            if (st == noErr) {
                uint32_t total = size / sizeof(AudioObjectID);
                if (total > 0) {
                    pid_t pid = 0;
                    UInt32 pid_size = sizeof(pid);
                    OSStatus pid_st = cspa_get_prop(objs[0], kAudioProcessPropertyPID,
                                                    kAudioObjectPropertyScopeGlobal, &pid_size, &pid);
                    if (cspa_is_tcc_denied_osstatus(pid_st)) {
                        return 2;
                    }
                }
            }
            return 0;
        }
        return -2;
    }
}

int cspa_mac_request_system_audio_tap(void) {
    @autoreleasepool {
        int pre = cspa_mac_system_audio_tap_preflight();
        if (pre == 2 || pre == -2) {
            return pre;
        }
        if (!cspa_mac_is_interactive_gui_session()) {
            return pre;
        }
        if (@available(macOS 14.2, *)) {
            CATapDescription *desc = [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:@[]];
            if (!desc) return 0;
            desc.name = @"CSPA Permission Probe";
            desc.privateTap = YES;
            desc.muteBehavior = CATapUnmuted;

            AudioObjectID tap = 0;
            OSStatus st = AudioHardwareCreateProcessTap(desc, &tap);
            if (cspa_is_tcc_denied_osstatus(st)) {
                return 2;
            }
            if (st != noErr || tap == 0) {
                return 0;
            }
            AudioObjectPropertyAddress addr = {
                kAudioTapPropertyDescription,
                kAudioObjectPropertyScopeGlobal,
                kAudioObjectPropertyElementMain
            };
            OSStatus set_st = AudioObjectSetPropertyData(tap, &addr, 0, NULL, sizeof(desc), &desc);
            AudioHardwareDestroyProcessTap(tap);
            if (cspa_is_tcc_denied_osstatus(set_st)) {
                return 2;
            }
            return (set_st == noErr) ? 3 : 0;
        }
        return -2;
    }
}

int cspa_mac_open_privacy_settings(int kind_code) {
    @autoreleasepool {
        if (!cspa_mac_is_interactive_gui_session()) {
            return 0;
        }
        NSString *url_str = nil;
        switch (kind_code) {
            case 0:
                url_str = @"x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
                break;
            case 1:
                url_str = @"x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture";
                break;
            case 2:
                url_str = @"x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
                break;
            default:
                return 0;
        }
        NSURL *url = [NSURL URLWithString:url_str];
        if (!url) return 0;
        return [[NSWorkspace sharedWorkspace] openURL:url] ? 1 : 0;
    }
}

static uint32_t cspa_count_scope_channels(AudioObjectID dev, AudioObjectPropertyScope scope) {
    AudioObjectPropertyAddress addr = {
        kAudioDevicePropertyStreamConfiguration,
        scope,
        kAudioObjectPropertyElementMain
    };
    UInt32 size = 0;
    if (AudioObjectGetPropertyDataSize(dev, &addr, 0, NULL, &size) != noErr || size < sizeof(UInt32)) {
        return 0;
    }
    AudioBufferList *abl = (AudioBufferList *)calloc(1, size);
    if (!abl) return 0;
    uint32_t total = 0;
    if (AudioObjectGetPropertyData(dev, &addr, 0, NULL, &size, abl) == noErr) {
        for (UInt32 i = 0; i < abl->mNumberBuffers; ++i) {
            total += abl->mBuffers[i].mNumberChannels;
        }
    }
    free(abl);
    return total;
}

static bool cspa_is_private_or_hidden_device(AudioObjectID dev, NSString *uid_str) {
    if (!uid_str || uid_str.length == 0) return true;
    if ([uid_str hasPrefix:@"CADefaultDeviceAggregate"] ||
        [uid_str hasPrefix:@"com.apple.audio.CoreAudioTap"] ||
        [uid_str hasPrefix:@"cspa-private-agg-"]) {
        return true;
    }
    UInt32 hidden = 0;
    UInt32 hsz = sizeof(hidden);
    if (cspa_get_prop(dev, kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal, &hsz, &hidden) == noErr && hidden != 0) {
        return true;
    }
    return false;
}

int cspa_mac_list_process_objects(CspaMacProcessObject *out_buf, uint32_t max_items, uint32_t *out_count) {
    @autoreleasepool {
        if (!out_buf || !out_count || max_items == 0) return -1;
        *out_count = 0;
        AudioObjectID objs[512];
        UInt32 size = sizeof(objs);
        OSStatus st = cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyProcessObjectList,
                                    kAudioObjectPropertyScopeGlobal, &size, objs);
        if (st != noErr) return (int)st;
        uint32_t total = size / sizeof(AudioObjectID);
        uint32_t written = 0;
        for (uint32_t i = 0; i < total && written < max_items; ++i) {
            AudioObjectID obj = objs[i];
            pid_t pid = 0;
            UInt32 pid_size = sizeof(pid);
            if (cspa_get_prop(obj, kAudioProcessPropertyPID, kAudioObjectPropertyScopeGlobal, &pid_size, &pid) != noErr || pid <= 0) {
                continue;
            }
            UInt32 running = 0;
            UInt32 run_size = sizeof(running);
            if (cspa_get_prop(obj, kAudioProcessPropertyIsRunningOutput, kAudioObjectPropertyScopeGlobal, &run_size, &running) != noErr) {
                running = 0;
            }
            CspaMacProcessObject *dst = &out_buf[written++];
            memset(dst, 0, sizeof(*dst));
            dst->object_id = (uint32_t)obj;
            dst->pid = (uint32_t)pid;
            dst->running_output = running ? 1u : 0u;
            CFStringRef bundle_ref = NULL;
            UInt32 bsize = sizeof(bundle_ref);
            if (cspa_get_prop(obj, kAudioProcessPropertyBundleID, kAudioObjectPropertyScopeGlobal, &bsize, &bundle_ref) == noErr && bundle_ref) {
                NSString *bstr = (__bridge_transfer NSString *)bundle_ref;
                const char *utf8 = bstr.UTF8String;
                if (utf8) {
                    strncpy(dst->bundle_id, utf8, sizeof(dst->bundle_id) - 1);
                }
            }
        }
        *out_count = written;
        return 0;
    }
}

int cspa_mac_translate_pid_to_process_object(uint32_t pid, uint32_t *out_object_id) {
    if (!out_object_id || pid == 0) return -1;
    *out_object_id = 0;
    pid_t target_pid = (pid_t)pid;
    AudioObjectID obj = 0;
    UInt32 size = sizeof(obj);
    AudioObjectPropertyAddress addr = {
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain
    };
    OSStatus st = AudioObjectGetPropertyData(kAudioObjectSystemObject, &addr,
                                             sizeof(target_pid), &target_pid, &size, &obj);
    if (st != noErr || obj == 0) return -2;
    *out_object_id = (uint32_t)obj;
    return 0;
}

int cspa_mac_list_output_devices(CspaMacOutputDevice *out_buf, uint32_t max_items, uint32_t *out_count) {
    @autoreleasepool {
        if (!out_buf || !out_count || max_items == 0) return -1;
        *out_count = 0;
        AudioObjectID default_dev = 0;
        UInt32 def_size = sizeof(default_dev);
        cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultOutputDevice,
                      kAudioObjectPropertyScopeGlobal, &def_size, &default_dev);

        AudioObjectID devs[128];
        UInt32 size = sizeof(devs);
        OSStatus st = cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDevices,
                                    kAudioObjectPropertyScopeGlobal, &size, devs);
        if (st != noErr) return (int)st;
        uint32_t total = size / sizeof(AudioObjectID);
        uint32_t written = 0;
        for (uint32_t i = 0; i < total && written < max_items; ++i) {
            AudioObjectID dev = devs[i];
            uint32_t out_ch = cspa_count_scope_channels(dev, kAudioObjectPropertyScopeOutput);
            if (out_ch == 0) {
                continue;
            }
            CFStringRef uid_ref = NULL;
            UInt32 uid_size = sizeof(uid_ref);
            if (cspa_get_prop(dev, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal, &uid_size, &uid_ref) != noErr || !uid_ref) {
                continue;
            }
            NSString *uid_str = (__bridge_transfer NSString *)uid_ref;
            if (cspa_is_private_or_hidden_device(dev, uid_str)) {
                continue;
            }
            CFStringRef name_ref = NULL;
            UInt32 name_size = sizeof(name_ref);
            NSString *name_str = uid_str;
            if (cspa_get_prop(dev, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, &name_size, &name_ref) == noErr && name_ref) {
                name_str = (__bridge_transfer NSString *)name_ref;
            }
            Float64 rate = 48000.0;
            UInt32 rate_size = sizeof(rate);
            cspa_get_prop(dev, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, &rate_size, &rate);

            CspaMacOutputDevice *dst = &out_buf[written++];
            memset(dst, 0, sizeof(*dst));
            dst->object_id = (uint32_t)dev;
            dst->is_default = (dev == default_dev) ? 1u : 0u;
            dst->nominal_rate_hz = (uint32_t)(rate > 0.0 ? rate : 48000.0);
            dst->channels = out_ch;
            if (uid_str.UTF8String) strncpy(dst->uid, uid_str.UTF8String, sizeof(dst->uid) - 1);
            if (name_str.UTF8String) strncpy(dst->name, name_str.UTF8String, sizeof(dst->name) - 1);
        }
        *out_count = written;
        return 0;
    }
}

int cspa_mac_list_input_devices(CspaMacInputDevice *out_buf, uint32_t max_items, uint32_t *out_count) {
    @autoreleasepool {
        if (!out_buf || !out_count || max_items == 0) return -1;
        *out_count = 0;
        AudioObjectID default_dev = 0;
        UInt32 def_size = sizeof(default_dev);
        cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultInputDevice,
                      kAudioObjectPropertyScopeGlobal, &def_size, &default_dev);

        AudioObjectID devs[128];
        UInt32 size = sizeof(devs);
        OSStatus st = cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDevices,
                                    kAudioObjectPropertyScopeGlobal, &size, devs);
        if (st != noErr) return (int)st;
        uint32_t total = size / sizeof(AudioObjectID);
        uint32_t written = 0;
        for (uint32_t i = 0; i < total && written < max_items; ++i) {
            AudioObjectID dev = devs[i];
            uint32_t in_ch = cspa_count_scope_channels(dev, kAudioObjectPropertyScopeInput);
            if (in_ch == 0) {
                continue;
            }
            CFStringRef uid_ref = NULL;
            UInt32 uid_size = sizeof(uid_ref);
            if (cspa_get_prop(dev, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal, &uid_size, &uid_ref) != noErr || !uid_ref) {
                continue;
            }
            NSString *uid_str = (__bridge_transfer NSString *)uid_ref;
            if (cspa_is_private_or_hidden_device(dev, uid_str)) {
                continue;
            }
            CFStringRef name_ref = NULL;
            UInt32 name_size = sizeof(name_ref);
            NSString *name_str = uid_str;
            if (cspa_get_prop(dev, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, &name_size, &name_ref) == noErr && name_ref) {
                name_str = (__bridge_transfer NSString *)name_ref;
            }
            Float64 rate = 48000.0;
            UInt32 rate_size = sizeof(rate);
            cspa_get_prop(dev, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, &rate_size, &rate);
            AudioStreamBasicDescription sfmt = {0};
            UInt32 fsz = sizeof(sfmt);
            if (cspa_get_prop(dev, kAudioDevicePropertyStreamFormat, kAudioObjectPropertyScopeInput, &fsz, &sfmt) == noErr && sfmt.mSampleRate > 0.0) {
                rate = sfmt.mSampleRate;
                if (sfmt.mChannelsPerFrame > 0) {
                    in_ch = (uint32_t)sfmt.mChannelsPerFrame;
                }
            }
            UInt32 transport = 0;
            UInt32 tsz = sizeof(transport);
            cspa_get_prop(dev, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal, &tsz, &transport);

            CspaMacInputDevice *dst = &out_buf[written++];
            memset(dst, 0, sizeof(*dst));
            dst->object_id = (uint32_t)dev;
            dst->is_default = (dev == default_dev) ? 1u : 0u;
            dst->nominal_rate_hz = (uint32_t)(rate > 0.0 ? rate : 48000.0);
            dst->channels = in_ch;
            dst->transport_type = (uint32_t)transport;
            if (uid_str.UTF8String) strncpy(dst->uid, uid_str.UTF8String, sizeof(dst->uid) - 1);
            if (name_str.UTF8String) strncpy(dst->name, name_str.UTF8String, sizeof(dst->name) - 1);
        }
        *out_count = written;
        return 0;
    }
}

int cspa_mac_classify_device_uid(const char *device_uid,
                                 uint32_t *out_object_id,
                                 uint32_t *out_in_channels,
                                 uint32_t *out_out_channels,
                                 uint32_t *out_rate_hz,
                                 uint32_t *out_transport_type) {
    @autoreleasepool {
        if (!device_uid || device_uid[0] == '\0') return -1;
        NSString *target_uid = [NSString stringWithUTF8String:device_uid];
        if (!target_uid || target_uid.length == 0) return -1;

        AudioObjectID devs[128];
        UInt32 size = sizeof(devs);
        if (cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDevices,
                          kAudioObjectPropertyScopeGlobal, &size, devs) != noErr) {
            return -1;
        }
        uint32_t total = size / sizeof(AudioObjectID);
        for (uint32_t i = 0; i < total; ++i) {
            AudioObjectID dev = devs[i];
            CFStringRef uid_ref = NULL;
            UInt32 uid_size = sizeof(uid_ref);
            if (cspa_get_prop(dev, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal, &uid_size, &uid_ref) != noErr || !uid_ref) {
                continue;
            }
            NSString *uid_str = (__bridge_transfer NSString *)uid_ref;
            if (![uid_str isEqualToString:target_uid]) {
                continue;
            }
            if (cspa_is_private_or_hidden_device(dev, uid_str)) {
                return -1;
            }
            uint32_t in_ch = cspa_count_scope_channels(dev, kAudioObjectPropertyScopeInput);
            uint32_t out_ch = cspa_count_scope_channels(dev, kAudioObjectPropertyScopeOutput);
            Float64 rate = 48000.0;
            UInt32 rsz = sizeof(rate);
            cspa_get_prop(dev, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, &rsz, &rate);
            UInt32 transport = 0;
            UInt32 tsz = sizeof(transport);
            cspa_get_prop(dev, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal, &tsz, &transport);
            if (out_object_id) *out_object_id = (uint32_t)dev;
            if (out_in_channels) *out_in_channels = in_ch;
            if (out_out_channels) *out_out_channels = out_ch;
            if (out_rate_hz) *out_rate_hz = (uint32_t)(rate > 0.0 ? rate : 48000.0);
            if (out_transport_type) *out_transport_type = (uint32_t)transport;
            return 0;
        }
        return -1;
    }
}

void cspa_mac_pump_runloop_ms(uint32_t ms) {
    @autoreleasepool {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, (double)ms / 1000.0, false);
    }
}

int cspa_mac_list_sck_applications(CspaMacSckApp *out_buf, uint32_t max_items, uint32_t *out_count) {
    @autoreleasepool {
        if (!out_buf || !out_count || max_items == 0) return -1;
        *out_count = 0;
        if (!CGPreflightScreenCaptureAccess()) return -2;
        dispatch_semaphore_t sema = dispatch_semaphore_create(0);
        __block SCShareableContent *content = nil;
        __block NSError *err = nil;
        [SCShareableContent getShareableContentExcludingDesktopWindows:YES
                                                   onScreenWindowsOnly:NO
                                                     completionHandler:^(SCShareableContent *val, NSError *error) {
            content = val;
            err = error;
            dispatch_semaphore_signal(sema);
        }];
        if (!cspa_wait_sema(sema, 5.0) || err || !content) return -3;
        uint32_t written = 0;
        for (SCRunningApplication *app in content.applications) {
            if (written >= max_items) break;
            if (app.processID <= 0) continue;
            CspaMacSckApp *dst = &out_buf[written++];
            memset(dst, 0, sizeof(*dst));
            dst->pid = (uint32_t)app.processID;
            if (app.bundleIdentifier.UTF8String) {
                strncpy(dst->bundle_id, app.bundleIdentifier.UTF8String, sizeof(dst->bundle_id) - 1);
            }
            if (app.applicationName.UTF8String) {
                strncpy(dst->app_name, app.applicationName.UTF8String, sizeof(dst->app_name) - 1);
            }
        }
        *out_count = written;
        return 0;
    }
}

int cspa_mac_list_running_applications(CspaMacRunningApp *out_buf, uint32_t max_items, uint32_t *out_count) {
    @autoreleasepool {
        if (!out_buf || !out_count || max_items == 0) return -1;
        *out_count = 0;
        NSArray<NSRunningApplication *> *apps = [[NSWorkspace sharedWorkspace] runningApplications];
        if (!apps) return -2;
        uint32_t written = 0;
        for (NSRunningApplication *app in apps) {
            if (written >= max_items) break;
            pid_t pid = app.processIdentifier;
            if (pid <= 0) continue;
            CspaMacRunningApp *dst = &out_buf[written++];
            memset(dst, 0, sizeof(*dst));
            dst->pid = (uint32_t)pid;
            if (app.bundleIdentifier.UTF8String) {
                strncpy(dst->bundle_id, app.bundleIdentifier.UTF8String, sizeof(dst->bundle_id) - 1);
            }
            if (app.localizedName.UTF8String) {
                strncpy(dst->app_name, app.localizedName.UTF8String, sizeof(dst->app_name) - 1);
            }
            if (app.executableURL.path.UTF8String) {
                strncpy(dst->exe_path, app.executableURL.path.UTF8String, sizeof(dst->exe_path) - 1);
            }
        }
        *out_count = written;
        return 0;
    }
}

typedef struct {
    AudioObjectID tap;
    AudioObjectID aggregate;
    AudioDeviceIOProcID io_proc;
    bool started;
    bool is_process_tap;
    uint32_t active_process_count;
    CspaAudioPacketCallback callback;
    void *user_data;
} CspaCoreAudioTapSession;

static OSStatus cspa_tap_io_proc(AudioObjectID device,
                                 const AudioTimeStamp *now,
                                 const AudioBufferList *input,
                                 const AudioTimeStamp *input_time,
                                 AudioBufferList *output,
                                 const AudioTimeStamp *output_time,
                                 void *client_data) {
    (void)device; (void)now; (void)output_time;
    if (output) {
        for (UInt32 i = 0; i < output->mNumberBuffers; ++i) {
            if (output->mBuffers[i].mData && output->mBuffers[i].mDataByteSize > 0) {
                memset(output->mBuffers[i].mData, 0, output->mBuffers[i].mDataByteSize);
            }
        }
    }
    CspaCoreAudioTapSession *sess = (CspaCoreAudioTapSession *)client_data;
    if (!sess || !sess->callback || !input || input->mNumberBuffers == 0) return noErr;
    if (sess->is_process_tap && __atomic_load_n(&sess->active_process_count, __ATOMIC_SEQ_CST) == 0) {
        return noErr;
    }

    uint32_t num_bufs = input->mNumberBuffers;
    if (num_bufs > 8) num_bufs = 8;
    const float *ptrs[8] = {0};
    uint32_t chans[8] = {0};
    uint32_t frames = 0;
    for (uint32_t i = 0; i < num_bufs; ++i) {
        const AudioBuffer *b = &input->mBuffers[i];
        if (!b->mData || b->mNumberChannels == 0 || b->mDataByteSize == 0) return noErr;
        uint32_t f = b->mDataByteSize / (sizeof(float) * b->mNumberChannels);
        if (i == 0) frames = f;
        else if (f != frames) return noErr;
        ptrs[i] = (const float *)b->mData;
        chans[i] = b->mNumberChannels;
    }
    if (frames == 0) return noErr;

    uint32_t ts_valid = (input_time && (input_time->mFlags & kAudioTimeStampHostTimeValid)) ? 1u : 0u;
    uint64_t ts_ns = ts_valid ? cspa_host_ticks_to_ns(input_time->mHostTime) : 0ULL;
    sess->callback(sess->user_data, num_bufs, ptrs, chans, frames, ts_ns, ts_valid);
    return noErr;
}

int cspa_mac_open_coreaudio_tap(const uint32_t *process_objects,
                                uint32_t process_count,
                                int is_process_tap,
                                const char *device_uid,
                                int mute_tap,
                                CspaAudioPacketCallback callback,
                                void *user_data,
                                uint32_t *out_rate_hz,
                                uint32_t *out_channels,
                                void **out_handle) {
    @autoreleasepool {
        if (!callback || !out_handle) return -1;
        *out_handle = NULL;
        if (cspa_mac_bundle_missing_usage_description("NSAudioCaptureUsageDescription")) {
            return -13;
        }
        if (@available(macOS 14.2, *)) {
            NSMutableArray<NSNumber *> *proc_array = [NSMutableArray arrayWithCapacity:process_count];
            for (uint32_t i = 0; i < process_count; ++i) {
                [proc_array addObject:@(process_objects[i])];
            }

            CATapDescription *desc = nil;
            NSString *resolved_uid = nil;
            if (device_uid && device_uid[0] != '\0') {
                if (is_process_tap == 0 && process_count == 0) {
                    uint32_t obj_id = 0, in_ch = 0, out_ch = 0, rate_hz = 0, transport = 0;
                    if (cspa_mac_classify_device_uid(device_uid, &obj_id, &in_ch, &out_ch, &rate_hz, &transport) != 0) {
                        return -11;
                    }
                    if (out_ch == 0 && in_ch > 0) {
                        return -10;
                    }
                }
                resolved_uid = [NSString stringWithUTF8String:device_uid];
            } else {
                AudioObjectID def_dev = 0;
                UInt32 def_size = sizeof(def_dev);
                if (cspa_get_prop(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultOutputDevice,
                                  kAudioObjectPropertyScopeGlobal, &def_size, &def_dev) == noErr && def_dev != 0) {
                    CFStringRef uid_ref = NULL;
                    UInt32 uid_size = sizeof(uid_ref);
                    if (cspa_get_prop(def_dev, kAudioDevicePropertyDeviceUID,
                                      kAudioObjectPropertyScopeGlobal, &uid_size, &uid_ref) == noErr && uid_ref) {
                        resolved_uid = (__bridge_transfer NSString *)uid_ref;
                    }
                }
            }
            if (is_process_tap != 0 || process_count > 0) {
                if (resolved_uid && resolved_uid.length > 0) {
                    desc = [[CATapDescription alloc] initWithProcesses:proc_array andDeviceUID:resolved_uid withStream:0];
                } else {
                    desc = [[CATapDescription alloc] initStereoMixdownOfProcesses:proc_array];
                }
                if (desc) desc.exclusive = NO;
            } else {
                if (resolved_uid && resolved_uid.length > 0) {
                    desc = [[CATapDescription alloc] initExcludingProcesses:@[] andDeviceUID:resolved_uid withStream:0];
                } else {
                    desc = [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:@[]];
                }
            }
            if (!desc) return -2;
            desc.name = @"CSPA CoreAudio Tap";
            desc.privateTap = YES;
            desc.muteBehavior = mute_tap ? CATapMuted : CATapUnmuted;

