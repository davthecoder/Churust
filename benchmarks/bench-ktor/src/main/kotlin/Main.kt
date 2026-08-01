// One of five halves of the comparison. The three routes here must stay
// byte-identical to every other bench app's — run.sh refuses to measure if
// they diverge.
//
// Ktor is in this comparison because Churust's whole shape — the builder, the
// pipeline phases, the `Call` — is borrowed from it. It is the design Churust
// is a Rust answer to, so it is the one framework here where losing would mean
// the answer was not worth writing.
//
// Netty rather than CIO: it is Ktor's default JVM engine and the one nearly
// every Ktor deployment runs. CIO is the Kotlin-native coroutine engine and is
// usually slower on the JVM.

import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.server.application.Application
import io.ktor.server.engine.embeddedServer
import io.ktor.server.netty.Netty
import io.ktor.server.response.respondText
import io.ktor.server.routing.get
import io.ktor.server.routing.routing

fun Application.module() {
    routing {
        get("/plaintext") {
            // ContentType.Text.Plain already carries `charset=UTF-8`, which
            // Ktor renders as `text/plain; charset=UTF-8` — capital UTF-8,
            // where every other app in this comparison writes it lowercase.
            // Spelled out here so the response bytes match rather than nearly
            // match; the equivalence gate compares header values exactly and a
            // near-match is a different response.
            call.respondText(
                "Hello, World!",
                ContentType.parse("text/plain; charset=utf-8"),
            )
        }

        get("/json") {
            // A constant string rather than kotlinx.serialization over a data
            // class, matching the other four apps: none of them runs a
            // serializer here either.
            call.respondText(
                """{"message":"Hello, World!"}""",
                ContentType.Application.Json,
            )
        }

        get("/user/{id}") {
            // toULongOrNull, because the other apps extract this as a u64 and
            // reject what will not parse. Skipping the parse would let Ktor do
            // less work per request than the frameworks it is compared with.
            val id = call.parameters["id"]?.toULongOrNull()
            if (id == null) {
                call.respondText(
                    "Bad Request",
                    ContentType.parse("text/plain; charset=utf-8"),
                    HttpStatusCode.BadRequest,
                )
            } else {
                call.respondText(
                    "user $id",
                    ContentType.parse("text/plain; charset=utf-8"),
                )
            }
        }
    }
}

fun main() {
    val port = System.getenv("PORT")?.toInt() ?: error("PORT must be set")
    embeddedServer(Netty, port = port, host = "127.0.0.1", module = Application::module)
        .start(wait = true)
}
