import meta from "../deno.json" with { type: "json" };
import { dlopen } from "@denosaurs/plug";

const symbols = {
  sc_set_global_project_dir: {
    parameters: [
      "buffer", // const char *project_dir
    ],
    result: "i32", // int
  },
  sc_set_global_should_collect: {
    parameters: [
      "function", // skycurrent_should_collect_callback_t should_collect
      "pointer", // void *user_data
    ],
    result: "void",
  },
  sc_init: {
    parameters: [],
    result: "void",
  },
  sc_send_stream: {
    parameters: [
      "buffer", // const uint8_t *data
      "usize", // size_t len
      "usize", // size_t header_size
    ],
    result: "void",
  },
  sc_iter_stream: {
    parameters: [],
    result: "pointer", // lmq_consumer_t *
  },
  sc_close: {
    parameters: [],
    result: "void",
  },
} as const satisfies Deno.ForeignLibraryInterface;

let lib: Deno.DynamicLibrary<typeof symbols>["symbols"];

function tryGetEnv(key: string): string | undefined {
  try {
    return Deno.env.get(key);
  } catch (e) {
    if (e instanceof Deno.errors.NotCapable || e instanceof Deno.errors.PermissionDenied) {
      return undefined;
    }
    throw e;
  }
}

try {
  const customPath = tryGetEnv("DENO_SC_PATH");
  const scLocal = tryGetEnv("DENO_SC_LOCAL");
  
  if (scLocal === "debug" || scLocal === "release") {
    lib = Deno.dlopen(
      new URL(
        `../../target/${scLocal}/${Deno.build.os === "windows" ? "" : "lib"}skycurrent.${
          Deno.build.os === "windows"
          ? "dll"
          : Deno.build.os === "darwin"
          ? "dylib"
          : "so"
        }`,
        import.meta.url
      ),
      symbols
    ).symbols;
  } else if (customPath) {
    lib = Deno.dlopen(customPath, symbols).symbols;
  } else {
    lib = (
      await dlopen(
        {
          name: "skycurrent",
          url: `${meta.github}/releases/download/${meta.tag}/`,
        },
        symbols
      )
    ).symbols;
  }
} catch (e) {
  if (e instanceof Deno.errors.NotCapable || e instanceof Deno.errors.PermissionDenied) {
    throw e;
  }
  
  throw new Error("Failed to load SkyCurrent Dynamic Library", { cause: e });
}

export default lib;

