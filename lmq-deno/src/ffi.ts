import meta from "../deno.json" with { type: "json" };
import { dlopen } from "@denosaurs/plug";

export const enum lmq_action_t {
  LMQ_ACTION_CONTINUE = 0,
  LMQ_ACTION_PAUSE = 1,
  LMQ_ACTION_DEREGISTER = -1,
}

export const enum lmq_resume_mode_t {
  LMQ_RESUME_WAIT = 0,
  LMQ_RESUME_TRY = 1,
}

const symbols = {
  lmq_new: {
    parameters: [],
    result: "pointer", // lmq_t *
  },
  lmq_destroy: {
    parameters: [
      "pointer", // lmq_t *queue
    ],
    result: "void",
  },
  lmq_push: {
    parameters: [
      "pointer", // lmq_t *queue
      "buffer", // const uint8_t *data
      "usize", // size_t len
    ],
    result: "void",
  },
  lmq_consumer_new: {
    parameters: [
      "pointer", // const lmq_t *queue
    ],
    result: "pointer", // lmq_consumer_t *
  },
  lmq_consumer_destroy: {
    parameters: [
      "pointer", // lmq_consumer_t *consumer
    ],
    result: "void",
  },
  lmq_consumer_try_next: {
    parameters: [
      "pointer", // lmq_consumer_t *consumer
    ],
    result: "pointer", // lmq_message_t *
  },
  lmq_consumer_wait: {
    parameters: [
      "pointer", // lmq_consumer_t *consumer
    ],
    result: "pointer", // lmq_message_t *
  },
  lmq_register_handler: {
    parameters: [
      "pointer", // const lmq_t *queue
      // typedef lmq_action_t (*lmq_msg_callback_t)(lmq_message_t *message, void *user_data);
      "function", // lmq_msg_callback_t callback
      "bool", // bool start_paused
      "pointer", // void *user_data
    ],
    result: "void",
  },
  lmq_resume_handler: {
    parameters: [
      "pointer", // const lmq_t *queue
      // typedef lmq_action_t (*lmq_msg_callback_t)(lmq_message_t *message, void *user_data);
      "function", // lmq_msg_callback_t callback
      "i32", // int no_block
    ],
    result: "i32", // int
  },
  lmq_deregister_handler: {
    parameters: [
      "pointer", // const lmq_t *queue
      // typedef lmq_action_t (*lmq_msg_callback_t)(lmq_message_t *message, void *user_data);
      "function", // lmq_msg_callback_t callback
    ],
    result: "bool",
  },
  lmq_message_destroy: {
    parameters: [
      "pointer", // lmq_message_t *message
    ],
    result: "void",
  },
  lmq_message_peek: {
    parameters: [
      "pointer", // const lmq_message_t *message,
      "buffer", // const uint8_t **p
      "buffer", // size_t *len
    ],
    result: "pointer", // lmq_message_ref_t *
  },
  lmq_message_peek_release: {
    parameters: [
      "pointer", // lmq_message_ref_t *message_ref
    ],
    result: "void",
  },
  lmq_message_claim: {
    parameters: [
      "pointer", // lmq_message_t *message
      "buffer", // uint8_t **p
      "buffer", // size_t *len
    ],
    result: "pointer", // lmq_vec_t *
  },
  lmq_vec_destroy: {
    parameters: [
      "pointer", // lmq_vec_t *data
    ],
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
  const customPath = tryGetEnv("DENO_LMQ_PATH");
  const lmqLocal = tryGetEnv("DENO_LMQ_LOCAL");
  
  if (lmqLocal === "debug" || lmqLocal === "release") {
    lib = Deno.dlopen(
      new URL(
        `../../target/${lmqLocal}/${Deno.build.os === "windows" ? "" : "lib"}lmq.${
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
          name: "lmq",
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
  
  throw new Error("Failed to load LMQ Dynamic Library", { cause: e });
}

export default lib;

