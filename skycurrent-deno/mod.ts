import ffi from "./src/ffi.ts";
import { MessageConsumer } from "@lmq/deno";

// Define types for global state.
type ShouldCollect = (header: Uint8Array) => boolean;
interface SkyCurrentStreamGlobals {
  globalShouldCollect?: Deno.UnsafeCallback;
}

// Ensure the global namespace exists.
(globalThis as any).__skycurrentstreamglobals = (globalThis as any).__skycurrentstreamglobals || {};
const globals = (globalThis as any).__skycurrentstreamglobals as SkyCurrentStreamGlobals;

/**
 * Sets the global project directory, required for certain backings.
 * 
 * @param projectDir Global project directory.
 * 
 * @throws Will throw an error if the argument is not a valid UTF-8 string.
 */
export function setGlobalProjectDir(projectDir: string) {
  const utf8 = new Uint8Array(toNullTerminatedUTF8Array(projectDir));
  if (ffi.sc_set_global_project_dir(utf8) == 1) {
    throw new Error("provided project directory is not a valid UTF-8 string!");
  }
}

/**
 * Sets the global `should_collect` callback, required for certain backings.
 * 
 * @param shouldCollect Callback that receives a read-only view of an incoming message's header, and decides if it should be collected.
 */
export function setGlobalShouldCollect(shouldCollect: ShouldCollect) {
  const callback = Deno.UnsafeCallback.threadSafe(
    {
      parameters: [
        "buffer", // const uint8_t *p
        "usize", // size_t len
        "pointer", // void *user_data
      ],
      result: "bool", // bool
    } as const,
    (dataPtr, dataLen, _user_data) => {
      if (dataPtr == null) {
        throw new Error("this should never happen.");
      }
      const dataView = new Deno.UnsafePointerView(
        dataPtr as Deno.PointerObject,
      ).getArrayBuffer(Number(dataLen));
      return shouldCollect(new Uint8Array(dataView));
    }
  );
  ffi.sc_set_global_should_collect(callback.pointer, null);
  if (globals.globalShouldCollect) {
    globals.globalShouldCollect.close();
  }
  globals.globalShouldCollect = callback as Deno.UnsafeCallback;
}

/**
 * Initialize SkyCurrent.
 */
export function init() {
  ffi.sc_init();
}

/**
 * Send an arbitrarily-sized payload
 * 
 * @param payload The data to send.
 * @param headerSize Size of the header section in bytes.
 */
export function sendStream(payload: Uint8Array | Array<number>, headerSize: number) {
  payload = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
  ffi.sc_send_stream(payload, BigInt(payload.byteLength), BigInt(headerSize));
}

/**
 * Get an iterator over the message stream.
 * 
 * This returns a [`MessageConsumer`] that can be used to iterate through and process unclaimed incoming messages.
 * 
 * The returned consumer must be freed with the `free` method after use.
 */
export function iterStream(): MessageConsumer {
  return MessageConsumer._createConsumerFromConsumerPointer(ffi.sc_iter_stream());
}

/**
 * Close SkyCurrent.
 */
export function close() {
  ffi.sc_close();
  if (globals.globalShouldCollect) {
    globals.globalShouldCollect.close();
    globals.globalShouldCollect = undefined;
  }
}

function toNullTerminatedUTF8Array(str: string) {
  const utf8 = [];
  for (let i = 0; i < str.length; i++) {
    let charcode = str.charCodeAt(i);
    if (charcode < 0x80) utf8.push(charcode);
    else if (charcode < 0x800) {
      utf8.push(0xc0 | (charcode >> 6), 0x80 | (charcode & 0x3f));
    } else if (charcode < 0xd800 || charcode >= 0xe000) {
      utf8.push(
        0xe0 | (charcode >> 12),
        0x80 | ((charcode >> 6) & 0x3f),
        0x80 | (charcode & 0x3f)
      );
    }
    // surrogate pair
    else {
      i++;
      // UTF-16 encodes 0x10000-0x10FFFF by
      // subtracting 0x10000 and splitting the
      // 20 bits of 0x0-0xFFFFF into two halves
      charcode =
        0x10000 + (((charcode & 0x3ff) << 10) | (str.charCodeAt(i) & 0x3ff));
      utf8.push(
        0xf0 | (charcode >> 18),
        0x80 | ((charcode >> 12) & 0x3f),
        0x80 | ((charcode >> 6) & 0x3f),
        0x80 | (charcode & 0x3f)
      );
    }
  }
  utf8.push(0x00);
  return utf8;
}

