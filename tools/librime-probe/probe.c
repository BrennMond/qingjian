/*
 * probe.c —— 用真实的 librime 跑一遍按键序列，把候选/上屏结果打成机器可读的记录。
 *
 * 为什么要手写 ABI：
 *   本机只装了 librime1t64 运行时（/usr/lib/x86_64-linux-gnu/librime.so.1），
 *   没有 librime-dev，因此既没有 /usr/include/rime_api.h，也没有 librime.so 符号链接。
 *   所以这里 dlopen 运行时库、dlsym 取符号，并在本文件里按 .scratch/rime_api.h
 *   （librime tag 1.16.1）逐字段重抄所需的结构体与函数指针表。
 *
 * 另一个实测事实（很重要）：
 *   这个 librime 构建 **没有** 导出 C 名字的 RimeSetup / RimeProcessKey / RimeGetContext …
 *   （它们只以 C++ mangled 名字存在，例如 _Z14RimeProcessKeymii）。
 *   唯一稳定的 C 入口是 `rime_get_api()`，返回一张版本化的函数指针表 RimeApi。
 *   因此本程序只用 rime_get_api()，其余调用一律走 api->xxx。
 *
 * 编译：cc -O2 -o probe probe.c -ldl
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <limits.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* ==================================================================== */
/* 以下结构体/宏逐字段抄自 .scratch/rime_api.h (librime 1.16.1)。          */
/* Linux 上 RIME_FLAVORED(X) 展开为 X，故直接用不带前缀的名字。            */
/* ==================================================================== */

#ifndef Bool
#define Bool int
#endif
#ifndef False
#define False 0
#endif
#ifndef True
#define True 1
#endif

typedef uintptr_t RimeSessionId;

/* 注意：1.16.1 的 RIME_STRUCT_INIT 是 sizeof(Type) - sizeof(data_size)，
 * 不是早期版本的 sizeof(Type)。必须照抄，否则 HAS_MEMBER 的判断会和
 * 库内部的判断不一致。 */
#define RIME_STRUCT_INIT(Type, var) \
  ((var).data_size = sizeof(Type) - sizeof((var).data_size))
#define RIME_STRUCT_HAS_MEMBER(var, member)           \
  ((int)(sizeof((var).data_size) + (var).data_size) > \
   (char*)&member - (char*)&var)
#define RIME_STRUCT_CLEAR(var) \
  memset((char*)&(var) + sizeof((var).data_size), 0, (var).data_size)
#define RIME_STRUCT(Type, var) \
  Type var = {0};              \
  RIME_STRUCT_INIT(Type, var);
#define RIME_API_AVAILABLE(api, func) \
  (RIME_STRUCT_HAS_MEMBER(*(api), (api)->func) && (api)->func)

typedef struct rime_traits_t {
  int data_size;
  const char* shared_data_dir;
  const char* user_data_dir;
  const char* distribution_name;
  const char* distribution_code_name;
  const char* distribution_version;
  const char* app_name;
  const char** modules;
  int min_log_level;
  const char* log_dir;
  const char* prebuilt_data_dir;
  const char* staging_dir;
} RimeTraits;

typedef struct {
  int length;
  int cursor_pos;
  int sel_start;
  int sel_end;
  char* preedit;
} RimeComposition;

typedef struct rime_candidate_t {
  char* text;
  char* comment;
  void* reserved;
} RimeCandidate;

typedef struct {
  int page_size;
  int page_no;
  Bool is_last_page;
  int highlighted_candidate_index;
  int num_candidates;
  RimeCandidate* candidates;
  char* select_keys;
} RimeMenu;

typedef struct rime_commit_t {
  int data_size;
  char* text;
} RimeCommit;

typedef struct rime_context_t {
  int data_size;
  RimeComposition composition;
  RimeMenu menu;
  char* commit_text_preview;
  char** select_labels;
} RimeContext;

typedef struct rime_status_t {
  int data_size;
  char* schema_id;
  char* schema_name;
  Bool is_disabled;
  Bool is_composing;
  Bool is_ascii_mode;
  Bool is_full_shape;
  Bool is_simplified;
  Bool is_traditional;
  Bool is_ascii_punct;
} RimeStatus;

typedef struct rime_candidate_list_iterator_t {
  void* ptr;
  int index;
  RimeCandidate candidate;
} RimeCandidateListIterator;

typedef struct rime_config_t {
  void* ptr;
} RimeConfig;

typedef struct rime_config_iterator_t {
  void* list;
  void* map;
  int index;
  const char* key;
  const char* path;
} RimeConfigIterator;

typedef struct rime_schema_list_item_t {
  char* schema_id;
  char* name;
  void* reserved;
} RimeSchemaListItem;

typedef struct rime_schema_list_t {
  size_t size;
  RimeSchemaListItem* list;
} RimeSchemaList;

typedef struct rime_string_slice_t {
  const char* str;
  size_t length;
} RimeStringSlice;

typedef void (*RimeNotificationHandler)(void* context_object,
                                        RimeSessionId session_id,
                                        const char* message_type,
                                        const char* message_value);

typedef struct rime_custom_api_t {
  int data_size;
} RimeCustomApi;

typedef struct rime_module_t {
  int data_size;
  const char* module_name;
  void (*initialize)(void);
  void (*finalize)(void);
  RimeCustomApi* (*get_api)(void);
} RimeModule;

/* RimeApi 的字段顺序必须与头文件完全一致：它是一张函数指针表，
 * 错一个字段就会调用到隔壁函数上。 */
typedef struct rime_api_t {
  int data_size;

  void (*setup)(RimeTraits* traits);
  void (*set_notification_handler)(RimeNotificationHandler handler,
                                   void* context_object);

  void (*initialize)(RimeTraits* traits);
  void (*finalize)(void);

  Bool (*start_maintenance)(Bool full_check);
  Bool (*is_maintenance_mode)(void);
  void (*join_maintenance_thread)(void);

  void (*deployer_initialize)(RimeTraits* traits);
  Bool (*prebuild)(void);
  Bool (*deploy)(void);
  Bool (*deploy_schema)(const char* schema_file);
  Bool (*deploy_config_file)(const char* file_name, const char* version_key);

  Bool (*sync_user_data)(void);

  RimeSessionId (*create_session)(void);
  Bool (*find_session)(RimeSessionId session_id);
  Bool (*destroy_session)(RimeSessionId session_id);
  void (*cleanup_stale_sessions)(void);
  void (*cleanup_all_sessions)(void);

  Bool (*process_key)(RimeSessionId session_id, int keycode, int mask);
  Bool (*commit_composition)(RimeSessionId session_id);
  void (*clear_composition)(RimeSessionId session_id);

  Bool (*get_commit)(RimeSessionId session_id, RimeCommit* commit);
  Bool (*free_commit)(RimeCommit* commit);
  Bool (*get_context)(RimeSessionId session_id, RimeContext* context);
  Bool (*free_context)(RimeContext* ctx);
  Bool (*get_status)(RimeSessionId session_id, RimeStatus* status);
  Bool (*free_status)(RimeStatus* status);

  void (*set_option)(RimeSessionId session_id, const char* option, Bool value);
  Bool (*get_option)(RimeSessionId session_id, const char* option);

  void (*set_property)(RimeSessionId session_id,
                       const char* prop,
                       const char* value);
  Bool (*get_property)(RimeSessionId session_id,
                       const char* prop,
                       char* value,
                       size_t buffer_size);

  Bool (*get_schema_list)(RimeSchemaList* schema_list);
  void (*free_schema_list)(RimeSchemaList* schema_list);

  Bool (*get_current_schema)(RimeSessionId session_id,
                             char* schema_id,
                             size_t buffer_size);
  Bool (*select_schema)(RimeSessionId session_id, const char* schema_id);

  Bool (*schema_open)(const char* schema_id, RimeConfig* config);
  Bool (*config_open)(const char* config_id, RimeConfig* config);
  Bool (*config_close)(RimeConfig* config);
  Bool (*config_get_bool)(RimeConfig* config, const char* key, Bool* value);
  Bool (*config_get_int)(RimeConfig* config, const char* key, int* value);
  Bool (*config_get_double)(RimeConfig* config, const char* key, double* value);
  Bool (*config_get_string)(RimeConfig* config,
                            const char* key,
                            char* value,
                            size_t buffer_size);
  const char* (*config_get_cstring)(RimeConfig* config, const char* key);
  Bool (*config_update_signature)(RimeConfig* config, const char* signer);
  Bool (*config_begin_map)(RimeConfigIterator* iterator,
                           RimeConfig* config,
                           const char* key);
  Bool (*config_next)(RimeConfigIterator* iterator);
  void (*config_end)(RimeConfigIterator* iterator);

  Bool (*simulate_key_sequence)(RimeSessionId session_id,
                                const char* key_sequence);

  Bool (*register_module)(RimeModule* module);
  RimeModule* (*find_module)(const char* module_name);

  Bool (*run_task)(const char* task_name);

  const char* (*get_shared_data_dir)(void);
  const char* (*get_user_data_dir)(void);
  const char* (*get_sync_dir)(void);

  const char* (*get_user_id)(void);
  void (*get_user_data_sync_dir)(char* dir, size_t buffer_size);

  Bool (*config_init)(RimeConfig* config);
  Bool (*config_load_string)(RimeConfig* config, const char* yaml);

  Bool (*config_set_bool)(RimeConfig* config, const char* key, Bool value);
  Bool (*config_set_int)(RimeConfig* config, const char* key, int value);
  Bool (*config_set_double)(RimeConfig* config, const char* key, double value);
  Bool (*config_set_string)(RimeConfig* config,
                            const char* key,
                            const char* value);

  Bool (*config_get_item)(RimeConfig* config,
                          const char* key,
                          RimeConfig* value);
  Bool (*config_set_item)(RimeConfig* config,
                          const char* key,
                          RimeConfig* value);
  Bool (*config_clear)(RimeConfig* config, const char* key);
  Bool (*config_create_list)(RimeConfig* config, const char* key);
  Bool (*config_create_map)(RimeConfig* config, const char* key);
  size_t (*config_list_size)(RimeConfig* config, const char* key);
  Bool (*config_begin_list)(RimeConfigIterator* iterator,
                            RimeConfig* config,
                            const char* key);

  const char* (*get_input)(RimeSessionId session_id);
  size_t (*get_caret_pos)(RimeSessionId session_id);
  Bool (*select_candidate)(RimeSessionId session_id, size_t index);
  const char* (*get_version)(void);
  void (*set_caret_pos)(RimeSessionId session_id, size_t caret_pos);
  Bool (*select_candidate_on_current_page)(RimeSessionId session_id,
                                           size_t index);
  Bool (*candidate_list_begin)(RimeSessionId session_id,
                               RimeCandidateListIterator* iterator);
  Bool (*candidate_list_next)(RimeCandidateListIterator* iterator);
  void (*candidate_list_end)(RimeCandidateListIterator* iterator);
  Bool (*user_config_open)(const char* config_id, RimeConfig* config);
  Bool (*candidate_list_from_index)(RimeSessionId session_id,
                                    RimeCandidateListIterator* iterator,
                                    int index);
  const char* (*get_prebuilt_data_dir)(void);
  const char* (*get_staging_dir)(void);
  void (*commit_proto)(RimeSessionId session_id, void* commit_builder);
  void (*context_proto)(RimeSessionId session_id, void* context_builder);
  void (*status_proto)(RimeSessionId session_id, void* status_builder);
  const char* (*get_state_label)(RimeSessionId session_id,
                                 const char* option_name,
                                 Bool state);
  Bool (*delete_candidate)(RimeSessionId session_id, size_t index);
  Bool (*delete_candidate_on_current_page)(RimeSessionId session_id,
                                           size_t index);
  RimeStringSlice (*get_state_label_abbreviated)(RimeSessionId session_id,
                                                 const char* option_name,
                                                 Bool state,
                                                 Bool abbreviated);
  Bool (*set_input)(RimeSessionId session_id, const char* input);

  void (*get_shared_data_dir_s)(char* dir, size_t buffer_size);
  void (*get_user_data_dir_s)(char* dir, size_t buffer_size);
  void (*get_prebuilt_data_dir_s)(char* dir, size_t buffer_size);
  void (*get_staging_dir_s)(char* dir, size_t buffer_size);
  void (*get_sync_dir_s)(char* dir, size_t buffer_size);

  Bool (*highlight_candidate)(RimeSessionId session_id, size_t index);
  Bool (*highlight_candidate_on_current_page)(RimeSessionId session_id,
                                              size_t index);

  Bool (*change_page)(RimeSessionId session_id, Bool backward);
} RimeApi;

/* 以上就是从 rime_api.h 抄来的全部内容。 */

/* ==================================================================== */
/* 小工具                                                                */
/* ==================================================================== */

#define PROBE_VERSION "1"
#define DEFAULT_SHARED_DIR "/usr/share/rime-data"
#define LIB_NAME "librime.so.1"
#define LIB_FALLBACK "/usr/lib/x86_64-linux-gnu/librime.so.1"

/* X11 keysym 常量，librime 的 keycode 沿用这一套。 */
#define KEY_SPACE 0x20
#define KEY_RETURN 0xFF0D
#define KEY_BACKSPACE 0xFF08
#define KEY_ESCAPE 0xFF1B
#define KEY_TAB 0xFF09
#define KEY_PAGE_UP 0xFF55
#define KEY_PAGE_DOWN 0xFF56
#define KEY_DELETE 0xFFFF
#define KEY_HOME 0xFF50
#define KEY_END 0xFF57
#define KEY_UP 0xFF52
#define KEY_DOWN 0xFF54
#define KEY_LEFT 0xFF51
#define KEY_RIGHT 0xFF53

/* librime 的修饰键掩码（rime/key_event.hh）：Shift=1 Lock=2 Control=4 Alt=8 */
#define MASK_SHIFT 1
#define MASK_LOCK 2
#define MASK_CONTROL 4
#define MASK_ALT 8

typedef struct {
  const char* name;
  int keycode;
} NamedKey;

static const NamedKey kNamedKeys[] = {
    {"space", KEY_SPACE},   {"Return", KEY_RETURN}, {"Enter", KEY_RETURN},
    {"BackSpace", KEY_BACKSPACE}, {"Escape", KEY_ESCAPE}, {"Tab", KEY_TAB},
    {"Page_Up", KEY_PAGE_UP}, {"Page_Down", KEY_PAGE_DOWN},
    {"Delete", KEY_DELETE}, {"Home", KEY_HOME},     {"End", KEY_END},
    {"Up", KEY_UP},         {"Down", KEY_DOWN},     {"Left", KEY_LEFT},
    {"Right", KEY_RIGHT},   {NULL, 0},
};

static void die(const char* fmt, ...) __attribute__((format(printf, 1, 2), noreturn));

static void die(const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  fputs("probe: ", stderr);
  vfprintf(stderr, fmt, ap);
  va_end(ap);
  fputc('\n', stderr);
  exit(1);
}

static void* xmalloc(size_t n) {
  void* p = malloc(n);
  if (!p)
    die("out of memory (%zu bytes)", n);
  return p;
}

static char* xstrdup(const char* s) {
  size_t n = strlen(s) + 1;
  char* p = xmalloc(n);
  memcpy(p, s, n);
  return p;
}

static int mkdir_p(const char* path) {
  char tmp[PATH_MAX];
  size_t len = strlen(path);
  if (len == 0 || len >= sizeof(tmp))
    return -1;
  memcpy(tmp, path, len + 1);
  if (tmp[len - 1] == '/')
    tmp[len - 1] = '\0';
  for (char* p = tmp + 1; *p; ++p) {
    if (*p == '/') {
      *p = '\0';
      if (mkdir(tmp, 0755) != 0 && errno != EEXIST)
        return -1;
      *p = '/';
    }
  }
  if (mkdir(tmp, 0755) != 0 && errno != EEXIST)
    return -1;
  return 0;
}

static int rm_rf(const char* path) {
  struct stat st;
  if (lstat(path, &st) != 0)
    return (errno == ENOENT) ? 0 : -1;
  if (!S_ISDIR(st.st_mode))
    return unlink(path);
  DIR* d = opendir(path);
  if (!d)
    return -1;
  struct dirent* e;
  int rc = 0;
  while ((e = readdir(d)) != NULL) {
    if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, ".."))
      continue;
    char child[PATH_MAX];
    if (snprintf(child, sizeof(child), "%s/%s", path, e->d_name) >=
        (int)sizeof(child)) {
      rc = -1;
      continue;
    }
    if (rm_rf(child) != 0)
      rc = -1;
  }
  closedir(d);
  if (rmdir(path) != 0)
    rc = -1;
  return rc;
}

static void msleep(long ms) {
  struct timespec ts;
  ts.tv_sec = ms / 1000;
  ts.tv_nsec = (ms % 1000) * 1000000L;
  nanosleep(&ts, NULL);
}

/* 设 PROBE_TRACE=1 时把每一步清理/部署动作打到 stderr，方便定位崩溃点。 */
static int g_trace = 0;

static void trace(const char* fmt, ...) {
  if (!g_trace)
    return;
  va_list ap;
  va_start(ap, fmt);
  fputs("probe[trace]: ", stderr);
  vfprintf(stderr, fmt, ap);
  va_end(ap);
  fputc('\n', stderr);
  fflush(stderr);
}

/* 可执行文件所在目录，用来把 run/ 放在 tools/librime-probe/ 下面，
 * 这样无论从哪个 cwd 调用都写到同一个位置。 */
static void exe_dir(char* out, size_t n) {
  ssize_t r = readlink("/proc/self/exe", out, n - 1);
  if (r <= 0)
    die("readlink(/proc/self/exe) failed: %s", strerror(errno));
  out[r] = '\0';
  char* slash = strrchr(out, '/');
  if (!slash)
    die("cannot determine executable directory from /proc/self/exe");
  *slash = '\0';
}

/* ==================================================================== */
/* JSON 输出                                                             */
/* ==================================================================== */

static void json_str(FILE* f, const char* s) {
  if (!s) {
    fputs("null", f);
    return;
  }
  fputc('"', f);
  for (const unsigned char* p = (const unsigned char*)s; *p; ++p) {
    switch (*p) {
      case '"': fputs("\\\"", f); break;
      case '\\': fputs("\\\\", f); break;
      case '\n': fputs("\\n", f); break;
      case '\r': fputs("\\r", f); break;
      case '\t': fputs("\\t", f); break;
      default:
        if (*p < 0x20)
          fprintf(f, "\\u%04x", *p);
        else
          fputc(*p, f); /* UTF-8 原样透出 */
    }
  }
  fputc('"', f);
}

/* 输出模式：默认每按一个键写一行 JSON；--json 时汇总成一个对象。 */
typedef struct {
  int aggregate;   /* 1 = 收集后合成单个 JSON 对象 */
  char** lines;
  size_t n, cap;
  FILE* out;
} Sink;

static void sink_init(Sink* s, int aggregate) {
  s->aggregate = aggregate;
  s->lines = NULL;
  s->n = s->cap = 0;
  s->out = stdout;
}

static void sink_line(Sink* s, char* owned_line) {
  if (!s->aggregate) {
    fputs(owned_line, s->out);
    fputc('\n', s->out);
    fflush(s->out);
    free(owned_line);
    return;
  }
  if (s->n == s->cap) {
    s->cap = s->cap ? s->cap * 2 : 16;
    s->lines = realloc(s->lines, s->cap * sizeof(char*));
    if (!s->lines)
      die("out of memory");
  }
  s->lines[s->n++] = owned_line;
}

static void sink_finish(Sink* s) {
  if (!s->aggregate) {
    fflush(s->out);
    return;
  }
  fputs("{\"event\":\"run\",\"records\":[\n", s->out);
  for (size_t i = 0; i < s->n; ++i) {
    fputs("  ", s->out);
    fputs(s->lines[i], s->out);
    fputs(i + 1 < s->n ? ",\n" : "\n", s->out);
    free(s->lines[i]);
  }
  fputs("]}\n", s->out);
  free(s->lines);
  s->lines = NULL;
  s->n = s->cap = 0;
  fflush(s->out);
}

/* ==================================================================== */
/* 命令行                                                                */
/* ==================================================================== */

typedef struct {
  const char* schema;
  const char* keys;
  int select_space;
  int page_down;
  int aggregate_json;
  int plain_text;
  int reset;
  int full_deploy;
  int verbose;
  const char* user_dir;
  const char* shared_dir;
  int deploy_timeout_s;
  int warmup_space;
} Options;

static void usage(const char* argv0) {
  fprintf(stderr,
          "用法: %s --schema <id> --keys \"<keys>\" [选项]\n"
          "\n"
          "  --schema ID        方案 id，例如 luna_pinyin / cangjie5（必填）\n"
          "  --keys KEYS        按键序列。普通字符 = 一次按键；\n"
          "                     也支持 <space> <Return> <BackSpace> <Escape>\n"
          "                     <Page_Down> <Page_Up> <Tab> <Delete> 等记号。\n"
          "                     例：--keys \"nihao\" / --keys \"ab<space>\"\n"
          "  --select-space     按键序列跑完后补一个空格键（选中高亮候选）\n"
          "  --page N           按键序列之后按 N 次 Page_Down（翻页）\n"
          "  --json             汇总成单个 JSON 对象（默认是每键一行的 JSONL）\n"
          "  --text             人类可读的纯文本输出\n"
          "  --reset            开跑前清空 run/user（强制重新部署）\n"
          "  --full-deploy      维护时做全量检查（默认增量，用系统预编译 .bin）\n"
          "  --user-dir DIR     覆盖一次性用户目录（默认 <exe目录>/run/user）\n"
          "  --shared-dir DIR   覆盖共享数据目录（默认 %s）\n"
          "  --warmup-space     按键序列前先按一次空格（用于清掉方案切换残留）\n"
          "  --deploy-timeout S 部署等待上限秒数（默认 600）\n"
          "  --verbose          打开 librime INFO 日志\n"
          "  --check-layout     打印手抄结构体的布局自检后退出\n"
          "  -h, --help         显示本帮助\n",
          argv0, DEFAULT_SHARED_DIR);
}

/* 把一个记号解析成 keycode+mask。返回 0 表示不是合法记号。 */
static int parse_named_key(const char* name, size_t len, int* keycode, int* mask) {
  char buf[64];
  if (len >= sizeof(buf))
    return 0;
  memcpy(buf, name, len);
  buf[len] = '\0';

  /* 允许 <Shift+Page_Down> 这类带修饰键的写法 */
  int m = 0;
  char* p = buf;
  for (;;) {
    if (!strncasecmp(p, "Shift+", 6)) {
      m |= MASK_SHIFT;
      p += 6;
    } else if (!strncasecmp(p, "Control+", 8)) {
      m |= MASK_CONTROL;
      p += 8;
    } else if (!strncasecmp(p, "Alt+", 4)) {
      m |= MASK_ALT;
      p += 4;
    } else {
      break;
    }
  }
  for (const NamedKey* k = kNamedKeys; k->name; ++k) {
    if (!strcasecmp(p, k->name)) {
      *keycode = k->keycode;
      *mask = m;
      return 1;
    }
  }
  if (strlen(p) == 1) {
    *keycode = (unsigned char)p[0];
    *mask = m;
    return 1;
  }
  return 0;
}

/* 一段按键序列 = 若干 (keycode, mask, 显示名) */
typedef struct {
  int keycode;
  int mask;
  char label[64];
} KeyStroke;

typedef struct {
  KeyStroke* v;
  size_t n, cap;
} KeySeq;

static void keyseq_push(KeySeq* s, int keycode, int mask, const char* label) {
  if (s->n == s->cap) {
    s->cap = s->cap ? s->cap * 2 : 32;
    s->v = realloc(s->v, s->cap * sizeof(KeyStroke));
    if (!s->v)
      die("out of memory");
  }
  s->v[s->n].keycode = keycode;
  s->v[s->n].mask = mask;
  snprintf(s->v[s->n].label, sizeof(s->v[s->n].label), "%s", label);
  s->n++;
}

/* 解析 --keys 字符串：普通字符逐字成键，<name> 成特殊键。 */
static void keyseq_parse(KeySeq* s, const char* keys) {
  const char* p = keys;
  while (*p) {
    if (*p == '<') {
      const char* close = strchr(p, '>');
      if (!close)
        die("按键序列里 '<' 没有闭合: %s", p);
      int kc = 0, mask = 0;
      if (!parse_named_key(p + 1, (size_t)(close - p - 1), &kc, &mask))
        die("无法识别的按键记号: <%.*s>", (int)(close - p - 1), p + 1);
      char label[64];
      snprintf(label, sizeof(label), "<%.*s>", (int)(close - p - 1), p + 1);
      keyseq_push(s, kc, mask, label);
      p = close + 1;
      continue;
    }
    unsigned char c = (unsigned char)*p;
    char label[8];
    if (c >= 'A' && c <= 'Z') {
      /* librime 期望小写 keycode + ShiftMask */
      label[0] = (char)c;
      label[1] = '\0';
      keyseq_push(s, (int)(c - 'A' + 'a'), MASK_SHIFT, label);
    } else if (c < 0x80) {
      label[0] = (char)c;
      label[1] = '\0';
      keyseq_push(s, (int)c, 0, label);
    } else {
      die("按键序列包含非 ASCII 字节 0x%02x；请用 <name> 记号表示特殊键", c);
    }
    p++;
  }
}

/* ==================================================================== */
/* librime 装载                                                          */
/* ==================================================================== */

typedef RimeApi* (*rime_get_api_fn)(void);

static RimeApi* load_rime(void** handle_out) {
  void* h = dlopen(LIB_NAME, RTLD_NOW | RTLD_LOCAL);
  if (!h) {
    const char* e1 = dlerror();
    fprintf(stderr, "probe: dlopen(\"%s\") 失败: %s\n", LIB_NAME, e1 ? e1 : "?");
    h = dlopen(LIB_FALLBACK, RTLD_NOW | RTLD_LOCAL);
    if (!h) {
      const char* e2 = dlerror();
      die("dlopen(\"%s\") 也失败: %s\n"
          "       请确认已安装 librime1t64（apt install librime1t64）。",
          LIB_FALLBACK, e2 ? e2 : "?");
    }
    fprintf(stderr, "probe: 已回退到绝对路径 %s\n", LIB_FALLBACK);
  }
  *handle_out = h;

  /* 本构建不导出 C 名字的 RimeSetup/RimeProcessKey…，
   * 唯一稳定的 C 入口是 rime_get_api()。 */
  dlerror();
  rime_get_api_fn get_api = (rime_get_api_fn)dlsym(h, "rime_get_api");
  if (!get_api)
    die("dlsym(\"rime_get_api\") 失败: %s", dlerror());
  RimeApi* api = get_api();
  if (!api)
    die("rime_get_api() 返回 NULL");
  if (api->data_size <= 0)
    die("rime_get_api() 返回的 RimeApi.data_size = %d，不可用", api->data_size);
  return api;
}

/* 布局自检：确认手抄的结构体尺寸/偏移和库的预期一致。
 * data_size 用错约定时，HAS_MEMBER 会立刻暴露出来。 */
static int check_layout(void) {
  int ok = 1;
  printf("sizeof(RimeTraits)  = %zu\n", sizeof(RimeTraits));
  printf("sizeof(RimeCommit)  = %zu\n", sizeof(RimeCommit));
  printf("sizeof(RimeContext) = %zu\n", sizeof(RimeContext));
  printf("sizeof(RimeStatus)  = %zu\n", sizeof(RimeStatus));
  printf("sizeof(RimeApi)     = %zu\n", sizeof(RimeApi));
  printf("offsetof(RimeContext, composition)        = %zu\n",
         offsetof(RimeContext, composition));
  printf("offsetof(RimeContext, menu)               = %zu\n",
         offsetof(RimeContext, menu));
  printf("offsetof(RimeContext, commit_text_preview)= %zu\n",
         offsetof(RimeContext, commit_text_preview));
  printf("offsetof(RimeContext, select_labels)      = %zu\n",
         offsetof(RimeContext, select_labels));
  printf("offsetof(RimeApi, process_key)            = %zu\n",
         offsetof(RimeApi, process_key));
  printf("offsetof(RimeApi, get_context)            = %zu\n",
         offsetof(RimeApi, get_context));
  printf("offsetof(RimeApi, select_schema)          = %zu\n",
         offsetof(RimeApi, select_schema));
  printf("offsetof(RimeApi, change_page)            = %zu\n",
         offsetof(RimeApi, change_page));

  RimeContext ctx = {0};
  RIME_STRUCT_INIT(RimeContext, ctx);
  printf("RimeContext.data_size after RIME_STRUCT_INIT = %d\n", ctx.data_size);
  printf("HAS_MEMBER(ctx, select_labels)               = %d\n",
         RIME_STRUCT_HAS_MEMBER(ctx, ctx.select_labels));
  if (!RIME_STRUCT_HAS_MEMBER(ctx, ctx.select_labels)) {
    printf("布局自检: 失败（select_labels 判定为不存在，data_size 约定不对）\n");
    ok = 0;
  }
  RimeTraits t = {0};
  RIME_STRUCT_INIT(RimeTraits, t);
  printf("HAS_MEMBER(traits, staging_dir)              = %d\n",
         RIME_STRUCT_HAS_MEMBER(t, t.staging_dir));
  if (!RIME_STRUCT_HAS_MEMBER(t, t.staging_dir))
    ok = 0;

  /* 最硬的外部校验：比较库自己填的 RimeApi.data_size 和我们手抄的结构体大小。
   * 两者一致 => 字段个数与顺序和库里那张函数指针表完全相同。 */
  void* h = NULL;
  RimeApi* api = load_rime(&h);
  int expect = (int)(sizeof(RimeApi) - sizeof(int));
  printf("库返回的 RimeApi.data_size = %d（本文件期望 %d）\n", api->data_size,
         expect);
  if (api->data_size != expect) {
    printf("布局自检: 失败（RimeApi 字段数与库不一致）\n");
    ok = 0;
  }
  if (RIME_API_AVAILABLE(api, get_version))
    printf("库版本: %s\n", api->get_version());
  dlclose(h);

  printf("布局自检: %s\n", ok ? "通过" : "失败");
  return ok ? 0 : 1;
}

/* ==================================================================== */
/* 上下文 / commit 输出                                                  */
/* ==================================================================== */

static char* ctx_to_json(const RimeContext* ctx, const RimeStatus* st) {
  char* buf = NULL;
  size_t len = 0;
  FILE* f = open_memstream(&buf, &len);
  if (!f)
    die("open_memstream failed");

  fputs("{\"preedit\":", f);
  json_str(f, ctx->composition.preedit ? ctx->composition.preedit : "");
  fprintf(f,
          ",\"cursor_pos\":%d,\"sel_start\":%d,\"sel_end\":%d"
          ",\"page_no\":%d,\"page_size\":%d,\"is_last_page\":%d"
          ",\"highlighted\":%d,\"num_candidates\":%d",
          ctx->composition.cursor_pos, ctx->composition.sel_start,
          ctx->composition.sel_end, ctx->menu.page_no, ctx->menu.page_size,
          ctx->menu.is_last_page ? 1 : 0,
          ctx->menu.highlighted_candidate_index, ctx->menu.num_candidates);

  fputs(",\"select_labels\":[", f);
  if (ctx->select_labels && RIME_STRUCT_HAS_MEMBER(*ctx, ctx->select_labels)) {
    for (int i = 0; i < ctx->menu.num_candidates; ++i) {
      if (i)
        fputc(',', f);
      json_str(f, ctx->select_labels[i]);
    }
  }
  fputc(']', f);

  fputs(",\"select_keys\":", f);
  json_str(f, ctx->menu.select_keys);

  fputs(",\"candidates\":[", f);
  for (int i = 0; i < ctx->menu.num_candidates; ++i) {
    const RimeCandidate* c = &ctx->menu.candidates[i];
    if (i)
      fputc(',', f);
    fprintf(f, "{\"index\":%d,\"text\":", i);
    json_str(f, c->text);
    fputs(",\"comment\":", f);
    json_str(f, c->comment);
    fputc('}', f);
  }
  fputc(']', f);

  fputs(",\"commit_text_preview\":", f);
  json_str(f, ctx->commit_text_preview);

  if (st) {
    fputs(",\"schema\":", f);
    json_str(f, st->schema_id);
    fprintf(f, ",\"is_composing\":%d,\"is_ascii_mode\":%d",
            st->is_composing ? 1 : 0, st->is_ascii_mode ? 1 : 0);
  }
  fputc('}', f);
  fclose(f);
  return buf;
}

/* 人类可读输出 */
static void print_text_record(size_t step,
                              const KeyStroke* k,
                              int handled,
                              const RimeContext* ctx,
                              const char* commit) {
  printf("[%zu] key=%s handled=%d preedit=\"%s\"\n", step, k->label, handled,
         ctx->composition.preedit ? ctx->composition.preedit : "");
  for (int i = 0; i < ctx->menu.num_candidates; ++i) {
    const RimeCandidate* c = &ctx->menu.candidates[i];
    printf("      %d. %s%s%s\n", i, c->text ? c->text : "",
           c->comment ? "  " : "", c->comment ? c->comment : "");
  }
  if (ctx->commit_text_preview && *ctx->commit_text_preview)
    printf("      preview: %s\n", ctx->commit_text_preview);
  if (commit && *commit)
    printf("      >>> COMMIT: %s\n", commit);
}

/* ==================================================================== */
/* main                                                                  */
/* ==================================================================== */

int main(int argc, char** argv) {
  Options opt;
  memset(&opt, 0, sizeof(opt));
  opt.deploy_timeout_s = 600;

  g_trace = getenv("PROBE_TRACE") ? 1 : 0;

  for (int i = 1; i < argc; ++i) {
    const char* a = argv[i];
    if (!strcmp(a, "--schema") && i + 1 < argc) {
      opt.schema = argv[++i];
    } else if (!strcmp(a, "--keys") && i + 1 < argc) {
      opt.keys = argv[++i];
    } else if (!strcmp(a, "--select-space")) {
      opt.select_space = 1;
    } else if (!strcmp(a, "--page") && i + 1 < argc) {
      opt.page_down = atoi(argv[++i]);
    } else if (!strcmp(a, "--json")) {
      opt.aggregate_json = 1;
    } else if (!strcmp(a, "--text")) {
      opt.plain_text = 1;
    } else if (!strcmp(a, "--reset")) {
      opt.reset = 1;
    } else if (!strcmp(a, "--full-deploy")) {
      opt.full_deploy = 1;
    } else if (!strcmp(a, "--verbose")) {
      opt.verbose = 1;
    } else if (!strcmp(a, "--warmup-space")) {
      opt.warmup_space = 1;
    } else if (!strcmp(a, "--user-dir") && i + 1 < argc) {
      opt.user_dir = argv[++i];
    } else if (!strcmp(a, "--shared-dir") && i + 1 < argc) {
      opt.shared_dir = argv[++i];
    } else if (!strcmp(a, "--deploy-timeout") && i + 1 < argc) {
      opt.deploy_timeout_s = atoi(argv[++i]);
    } else if (!strcmp(a, "--check-layout")) {
      return check_layout();
    } else if (!strcmp(a, "-h") || !strcmp(a, "--help")) {
      usage(argv[0]);
      return 0;
    } else {
      usage(argv[0]);
      die("未知参数: %s", a);
    }
  }

  if (!opt.schema)
    die("缺少 --schema（例：--schema luna_pinyin）");
  if (!opt.keys && !opt.warmup_space)
    die("缺少 --keys");
  if (opt.page_down < 0)
    die("--page 不能为负数");

  /* ---- 目录准备：一次性 user dir，绝不碰用户真实 RIME 配置 ---- */
  char dirbuf[PATH_MAX];
  char shared_dir[PATH_MAX];
  char user_dir[PATH_MAX];
  char prebuilt_dir[PATH_MAX];
  char staging_dir[PATH_MAX];
  char log_dir[PATH_MAX];

  if (opt.shared_dir) {
    snprintf(shared_dir, sizeof(shared_dir), "%s", opt.shared_dir);
  } else {
    snprintf(shared_dir, sizeof(shared_dir), "%s", DEFAULT_SHARED_DIR);
  }
  if (opt.user_dir) {
    snprintf(user_dir, sizeof(user_dir), "%s", opt.user_dir);
  } else {
    exe_dir(dirbuf, sizeof(dirbuf));
    snprintf(user_dir, sizeof(user_dir), "%s/run/user", dirbuf);
  }
  snprintf(prebuilt_dir, sizeof(prebuilt_dir), "%s/build", shared_dir);
  snprintf(staging_dir, sizeof(staging_dir), "%s/build", user_dir);
  snprintf(log_dir, sizeof(log_dir), "%s/log", user_dir);

  struct stat st;
  if (stat(shared_dir, &st) != 0 || !S_ISDIR(st.st_mode))
    die("共享数据目录不存在: %s", shared_dir);

  if (opt.reset && rm_rf(user_dir) != 0)
    die("--reset 清理 %s 失败: %s", user_dir, strerror(errno));
  if (mkdir_p(user_dir) != 0)
    die("无法创建用户目录 %s: %s", user_dir, strerror(errno));
  if (mkdir_p(log_dir) != 0)
    die("无法创建日志目录 %s: %s", log_dir, strerror(errno));

  /* glog 的日志文件名固定是 <程序名>.<主机>.<用户>.log.<级别>.<时间戳>.<pid>。
   * 同一秒内如果 pid 被复用（容器/PID namespace 下很常见），glog 的
   * open(O_EXCL) 就会撞名，往 stderr 吐一行
   * "Could not create logging file: File exists"。给每次运行一个 mkdtemp
   * 出来的专属目录就彻底避开了这个撞名。 */
  {
    char tmpl[PATH_MAX];
    snprintf(tmpl, sizeof(tmpl), "%s/probe-XXXXXX", log_dir);
    char* made = mkdtemp(tmpl);
    if (made)
      snprintf(log_dir, sizeof(log_dir), "%s", made);
    else
      fprintf(stderr, "probe: 警告：mkdtemp 失败，日志直接写进 %s\n", log_dir);
  }

  /* ---- 装载 librime ---- */
  void* handle = NULL;
  RimeApi* api = load_rime(&handle);

  /* ---- 按键序列 ---- */
  KeySeq seq;
  memset(&seq, 0, sizeof(seq));
  if (opt.keys)
    keyseq_parse(&seq, opt.keys);
  for (int i = 0; i < opt.page_down; ++i)
    keyseq_push(&seq, KEY_PAGE_DOWN, 0, "<Page_Down>");
  if (opt.select_space)
    keyseq_push(&seq, KEY_SPACE, 0, "<space>");

  Sink sink;
  sink_init(&sink, opt.aggregate_json && !opt.plain_text);

  /* ---- Traits：全部指向一次性目录 ---- */
  RimeTraits traits;
  memset(&traits, 0, sizeof(traits));
  RIME_STRUCT_INIT(RimeTraits, traits);
  traits.shared_data_dir = shared_dir;
  traits.user_data_dir = user_dir;
  traits.distribution_name = "Stele IME librime probe";
  traits.distribution_code_name = "stele-librime-probe";
  traits.distribution_version = PROBE_VERSION;
  traits.app_name = "rime.stele_probe";
  traits.modules = NULL;
  traits.min_log_level = opt.verbose ? 0 : 2;
  traits.log_dir = log_dir;
  traits.prebuilt_data_dir = prebuilt_dir;
  traits.staging_dir = staging_dir;

  if (RIME_API_AVAILABLE(api, setup))
    api->setup(&traits);
  if (!RIME_API_AVAILABLE(api, initialize))
    die("RimeApi->initialize 不可用（库版本过旧？）");
  api->initialize(&traits);

  const char* ver = RIME_API_AVAILABLE(api, get_version) ? api->get_version() : NULL;

  /* ---- 部署 ---- */
  if (RIME_API_AVAILABLE(api, start_maintenance)) {
    /* 增量部署即可：系统自带的 prebuilt .bin 已经是最新的，
     * 全量检查会白白重编译十几 MB 的表。 */
    Bool started = api->start_maintenance(opt.full_deploy ? True : False);
    if (started && RIME_API_AVAILABLE(api, is_maintenance_mode)) {
      long waited = 0;
      while (api->is_maintenance_mode() && waited < opt.deploy_timeout_s * 1000L) {
        msleep(50);
        waited += 50;
      }
      if (api->is_maintenance_mode())
        fprintf(stderr, "probe: 警告：部署超过 %d 秒仍在进行\n", opt.deploy_timeout_s);
    }
    if (RIME_API_AVAILABLE(api, join_maintenance_thread))
      api->join_maintenance_thread();
  } else if (RIME_API_AVAILABLE(api, prebuild)) {
    if (!api->prebuild())
      die("prebuild 失败");
  }

  /* ---- 会话 ---- */
  RimeSessionId session = api->create_session();
  if (!session)
    die("create_session 返回 0，无法建立输入会话");

  int exit_code = 0;
  char commit_all[4096];
  commit_all[0] = '\0';
  size_t commit_len = 0;
  size_t step = 0;

  /* 实测坑：librime 的 select_schema() 对一个**不存在**的方案 id 也会返回 True，
   * 并且 get_current_schema() 会把请求的 id 原样回显。也就是说光看这两个调用
   * 根本发现不了拼错的方案名 —— 会话会进入"没有引擎"的假死状态，按键全部
   * handled=0、候选恒为空，而退出码仍是 0。
   * 所以必须自己拿已部署方案列表核对一遍。 */
  if (RIME_API_AVAILABLE(api, get_schema_list) &&
      RIME_API_AVAILABLE(api, free_schema_list)) {
    RimeSchemaList list;
    memset(&list, 0, sizeof(list));
    if (api->get_schema_list(&list)) {
      int found = 0;
      for (size_t i = 0; i < list.size; ++i) {
        if (list.list[i].schema_id && !strcmp(list.list[i].schema_id, opt.schema))
          found = 1;
      }
      if (!found) {
        fprintf(stderr, "probe: 方案 \"%s\" 不在已部署的方案列表里。可用方案：\n",
                opt.schema);
        for (size_t i = 0; i < list.size; ++i)
          fprintf(stderr, "        %-20s %s\n", list.list[i].schema_id,
                  list.list[i].name ? list.list[i].name : "");
        api->free_schema_list(&list);
        exit_code = 1;
        goto cleanup;
      }
      api->free_schema_list(&list);
    }
  }

  if (!api->select_schema(session, opt.schema)) {
    fprintf(stderr, "probe: select_schema(\"%s\") 失败\n", opt.schema);
    exit_code = 1;
    goto cleanup;
  }

  /* ---- 会话头 ---- */
  {
    char active[256] = {0};
    if (RIME_API_AVAILABLE(api, get_current_schema))
      api->get_current_schema(session, active, sizeof(active));

    char* buf = NULL;
    size_t len = 0;
    FILE* f = open_memstream(&buf, &len);
    if (!f)
      die("open_memstream failed");
    fputs("{\"event\":\"session\",\"probe_version\":\"" PROBE_VERSION "\",\"librime\":", f);
    json_str(f, ver);
    fputs(",\"schema_requested\":", f);
    json_str(f, opt.schema);
    fputs(",\"schema_active\":", f);
    json_str(f, active);
    fputs(",\"keys\":", f);
    json_str(f, opt.keys ? opt.keys : "");
    fprintf(f, ",\"select_space\":%d,\"page_down\":%d", opt.select_space ? 1 : 0,
            opt.page_down);
    /* 这条很重要：user_data_dir 是**有状态**的。librime 会把上屏过的词写进
     * <schema>.userdb 并据此调整后续候选顺序，所以「冷基线」和「跑过几次之后」
     * 的输出是不一样的。reset=1 表示这次是从空目录开始的干净基线。 */
    fprintf(f, ",\"reset\":%d", opt.reset ? 1 : 0);
    fputs(",\"shared_data_dir\":", f);
    json_str(f, shared_dir);
    fputs(",\"user_data_dir\":", f);
    json_str(f, user_dir);
    fputs(",\"prebuilt_data_dir\":", f);
    json_str(f, prebuilt_dir);
    fputs(",\"staging_dir\":", f);
    json_str(f, staging_dir);
    fputc('}', f);
    fclose(f);

    if (opt.plain_text) {
      printf("librime %s  schema=%s  user_dir=%s\n", ver ? ver : "?",
             active[0] ? active : opt.schema, user_dir);
      free(buf);
    } else {
      sink_line(&sink, buf);
    }
  }

  if (strcmp(opt.schema, "") && RIME_API_AVAILABLE(api, get_current_schema)) {
    char active[256] = {0};
    api->get_current_schema(session, active, sizeof(active));
    if (active[0] && strcmp(active, opt.schema))
      die("请求的方案是 %s，但会话里实际生效的是 %s", opt.schema, active);
  }

  if (opt.warmup_space) {
    api->process_key(session, KEY_SPACE, 0);
    /* 丢掉 warmup 产生的 commit */
    RimeCommit c;
    memset(&c, 0, sizeof(c));
    RIME_STRUCT_INIT(RimeCommit, c);
    if (api->get_commit(session, &c))
      api->free_commit(&c);
  }

  /* ---- 主循环：一键一条记录 ---- */
  for (size_t i = 0; i < seq.n; ++i) {
    const KeyStroke* k = &seq.v[i];
    Bool handled = api->process_key(session, k->keycode, k->mask);

    /* 读 commit */
    char commit[1024];
    commit[0] = '\0';
    RimeCommit c;
    memset(&c, 0, sizeof(c));
    RIME_STRUCT_INIT(RimeCommit, c);
    if (api->get_commit(session, &c)) {
      if (c.text)
        snprintf(commit, sizeof(commit), "%s", c.text);
      api->free_commit(&c);
      if (commit[0]) {
        size_t n = strlen(commit);
        if (commit_len + n + 1 < sizeof(commit_all)) {
          memcpy(commit_all + commit_len, commit, n + 1);
          commit_len += n;
        }
      }
    }

    /* 读 context */
    RimeContext ctx;
    memset(&ctx, 0, sizeof(ctx));
    RIME_STRUCT_INIT(RimeContext, ctx);
    int have_ctx = api->get_context(session, &ctx);

    RimeStatus stbuf;
    RimeStatus* stp = NULL;
    if (RIME_API_AVAILABLE(api, get_status)) {
      memset(&stbuf, 0, sizeof(stbuf));
      RIME_STRUCT_INIT(RimeStatus, stbuf);
      if (api->get_status(session, &stbuf))
        stp = &stbuf;
    }

    if (opt.plain_text) {
      if (have_ctx)
        print_text_record(step, k, handled, &ctx, commit);
      else
        printf("[%zu] key=%s handled=%d (no context)\n", step, k->label, handled);
    } else {
      char* ctxjson = have_ctx ? ctx_to_json(&ctx, stp)
                               : xstrdup("{\"error\":\"get_context failed\"}");
      char* buf = NULL;
      size_t len = 0;
      FILE* f = open_memstream(&buf, &len);
      if (!f)
        die("open_memstream failed");
      fprintf(f, "{\"event\":\"key\",\"step\":%zu,\"key\":", step);
      json_str(f, k->label);
      fprintf(f, ",\"keycode\":%d,\"mask\":%d,\"handled\":%d,\"context\":%s,\"commit\":",
              k->keycode, k->mask, handled ? 1 : 0, ctxjson);
      if (commit[0])
        json_str(f, commit);
      else
        fputs("null", f);
      fputc('}', f);
      fclose(f);
      free(ctxjson);
      sink_line(&sink, buf);
    }

    if (have_ctx && api->free_context)
      api->free_context(&ctx);
    if (stp && RIME_API_AVAILABLE(api, free_status))
      api->free_status(stp);
    step++;
  }

  /* ---- 收尾 ---- */
  if (!opt.plain_text) {
    char* buf = NULL;
    size_t len = 0;
    FILE* f = open_memstream(&buf, &len);
    if (!f)
      die("open_memstream failed");
    fprintf(f, "{\"event\":\"end\",\"steps\":%zu,\"commit\":", step);
    if (commit_all[0])
      json_str(f, commit_all);
    else
      fputs("null", f);
    fputc('}', f);
    fclose(f);
    sink_line(&sink, buf);
  } else {
    printf("COMMIT: %s\n", commit_all[0] ? commit_all : "(none)");
  }

cleanup:
  trace("cleanup: destroy_session");
  if (session)
    api->destroy_session(session);
  trace("cleanup: finalize");
  if (RIME_API_AVAILABLE(api, finalize))
    api->finalize();
  trace("cleanup: sink_finish");
  sink_finish(&sink);
  /* 有意 **不** 调用 dlclose()：
   * librime 内的 glog 注册了 atexit / 静态析构回调；dlclose 之后再从 main
   * 返回，进程退出时会跳到已经 unmap 的代码上 —— 实测必崩
   * （dmesg 里 "segfault at <addr> ip <同一个 addr>"，即跳转到未映射页）。
   * 本进程马上就要退出，把库留在映射里是最省事也最安全的做法。 */
  trace("cleanup: done (dlclose intentionally skipped)");
  (void)handle;
  free(seq.v);

  /* 成功时清掉本次运行的日志目录；失败或 --verbose 时留着便于事后排查。
   * 必须在 finalize() 之后删，否则 glog 还在往里面写。 */
  if (exit_code == 0 && !opt.verbose)
    rm_rf(log_dir);

  if (exit_code == 0 && !commit_all[0] && opt.select_space)
    fprintf(stderr, "probe: 提示：--select-space 之后没有拿到 commit 文本\n");

  return exit_code;
}
