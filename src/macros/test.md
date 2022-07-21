Mark an `async fn` as a test with SQLx support.

The test will automatically be executed in the async runtime according to the chosen 
`runtime-{async-std, tokio}-{native-tls, rustls}` feature.

```rust,norun
# #[cfg(feature = "postgres")]
# mod example {
use sqlx::PgPool;
    
#[sqlx::test]
async fn test_it_connects(pool: PgPool) -> sqlx::Result<()> {
    
} 
# }

```

### Automatic Test Database Management

`#[sqlx::test]` can automatically create test databases for you and provide live connections to your test
if you set `DATABASE_URL` as an environment variable or in a `.env` file like `sqlx::query!()` _et al_.

For every annotated function, a new test database is created and migrated so tests can run against a live database
but are isolated from each other.

This feature is activated by changing the signature of your test function. The following signatures are supported:

* `async fn(Pool<DB>) -> Ret`
  * the `Pool`s used by all running tests share a single connection limit to avoid exceeding the server's limit.
* `async fn(PoolConnection<DB>) -> Ret`
* `async fn(&mut DB::Connection) -> Ret`
  * e.g. `&mut PgConnection`, `&mut MySqlConnection`, `&mut SqliteConnection`, etc.
* `async fn(PoolOptions<DB>, impl ConnectOptions<DB>) -> Ret`
    * Where `impl ConnectOptions` is, e.g, `PgConnectOptions`, `MySqlConnectOptions`, etc.
    * If your test wants to create its own `Pool` (for example, to set pool callbacks or to modify `ConnectOptions`), 
      you can use this signature.

Where `DB` is a supported `Database` type and `Ret` is any return type supported by the regular `#[test]` attribute,
except `ExitCode`.

Test databases are automatically cleaned up as tests succeed, but failed tests will leave their databases in-place
to facilitate debugging. Note that to simplify the implementation, panics are _always_ considered to be failures,
even for `#[should_panic]` tests.

If you have `sqlx-cli` installed, you can run `sqlx test-db cleanup` to delete all test databases. 
Old test databases will also be deleted the next time a test binary using `#[sqlx::test]` is run. 

### Automatic Migrations

To ensure a straightforward test implementation, migrations are automatically applied if a `migrations` folder
is found in the same directory as `CARGO_MANIFEST_DIR`.

