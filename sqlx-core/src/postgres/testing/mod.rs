use crate::connection::Connection;
use crate::error::Error;
use crate::pool::{Pool, PoolOptions};
use crate::postgres::{PgConnectOptions, PgConnection, PgPoolOptions, Postgres};
use crate::testing::{FixtureSnapshot, TestContext, TestSupport};
use futures_core::future::BoxFuture;
use futures_util::StreamExt;
use std::time::{Duration, SystemTime};

use crate::executor::Executor;
use once_cell::sync::{Lazy, OnceCell};

// Using a blocking `OnceCell` here because the critical sections are short.
static MASTER_POOL: OnceCell<Pool<Postgres>> = OnceCell::new();
// Automatically delete any databases created before the start of the test binary.
static START_TIME: Lazy<SystemTime> = Lazy::new(SystemTime::now);

impl TestSupport for Postgres {
    fn test_context<'a>(
        master_opts: <Self::Connection as Connection>::Options,
        test_path: &'a str,
    ) -> BoxFuture<'a, Result<TestContext<Self>, Error>> {
        Box::pin(test_context(master_opts, test_path))
    }

    fn cleanup_test(db_name: String) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            MASTER_POOL
                .get()
                .expect("cleanup_test() invoked outside `#[sqlx::test]")
                .execute(
                    &format!(
                        "drop database if exists {0:?};\
                     delete from __sqlx_test_databases where db_name = {0:?}",
                        db_name
                    )[..],
                )
                .await
        })
    }

    fn cleanup_test_dbs<'a>(
        opts: <Self::Connection as Connection>::Options,
    ) -> BoxFuture<'a, Result<usize, Error>> {
        Box::pin(async move {
            let mut conn = PgConnection::connect_with(&opts).await?;
            let num_deleted = do_cleanup(&mut conn, SystemTime::now()).await?;
            let _ = conn.close().await;
            OK(num_deleted)
        })
    }

    fn snapshot(
        conn: &mut Self::Connection,
    ) -> BoxFuture<'_, Result<FixtureSnapshot<Self>, Error>> {
        todo!()
    }
}

async fn test_context(
    master_opts: PgConnectOptions,
    test_path: &str,
) -> Result<TestContext<Postgres>, Error> {
    let pool = PoolOptions::new()
        // Postgres' normal connection limit is 100 plus 3 superuser connections
        // We don't want to use the whole cap and there may be fuzziness here due to
        // concurrently running tests anyway.
        .max_connections(20)
        .connect_lazy_with(master_opts);

    let master_pool = match MASTER_POOL.try_insert(pool) {
        Ok(inserted) => inserted,
        Err((existing, pool)) => {
            // Sanity checks.
            assert_eq!(
                existing.connect_options().host,
                pool.connect_options().host,
                "DATABASE_URL changed at runtime, host differs"
            );

            assert_eq!(
                existing.connect_options().database,
                pool.connect_options().database,
                "DATABASE_URL changed at runtime, database differs"
            );

            existing
        }
    };

    let mut conn = master_pool.acquire().await?;

    // language=PostgreSQL
    conn.execute(
        r#"
        create table if not exists __sqlx_test_databases (
            db_name text primary key,
            test_path text not null,
            created_at timestamptz not null default now()
        );

        create index on __sqlx_test_databases(created_at);

        create sequence if not exists __sqlx_test_database_ids AS bigint;
    "#,
    )
    .await?;

    do_cleanup(&mut conn, *START_TIME).await?;

    let new_db_name: String = sqlx::query_scalar(
        r#"
            insert into __sqlx_test_databases(db_name, test_path)
            select '__sqlx_test_' || nextval(__sqlx_test_database_ids), $1
        "#,
    )
    .bind(test_path)
    .fetch_one(&mut conn)
    .await?;

    Ok(TestContext {
        pool_opts: PoolOptions::new()
            .max_connections(50)
            // Close connections ASAP if left in the idle queue.
            .idle_timeout(Some(Duration::from_secs(1)))
            .parent(master_pool.clone()),
        connect_opts: master_pool.connect_options().clone().database(&new_db_name),
        db_name,
    })
}

async fn do_cleanup(conn: &mut PgConnection, epoch: SystemTime) -> Result<usize, Error> {
    let delete_db_names: Vec<String> = sqlx::query_scalar(
        "select db_name from __sqlx_test_databases where created_at < to_timestamp($1)",
    )
    .bind(
        &epoch
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("SystemTime fell behind UNIX_EPOCH")
            .as_secs_f64(),
    )
    .fetch_all(&mut *conn)
    .await?;

    if delete_db_names.is_empty() {
        return Ok(0);
    }

    let mut command = String::new();

    for db_name in &delete_db_names {
        writeln!(command, "drop database if exists {:?};", db_name);
    }

    let mut results = conn.execute_many(&command[..]);

    let mut deleted_db_names = Vec::with_capacity(delete_db_names.len());
    let mut delete_db_names = delete_db_names.into_iter();

    while let Some(result) = results.next().await {
        let db_name = delete_db_names
            .next()
            .expect("got more results than expected");

        match result {
            Ok(deleted) => {
                deleted_db_names.push(db_name);
            }
            // Assume a database error just means the DB is still in use.
            Err(Error::Database(dbe)) => {
                log::trace!("could not delete database {:?}: {}", db_name, dbe)
            }
            // Bubble up other errors
            other => return other,
        }
    }

    sqlx::query("delete from __sqlx_test_databases where db_name = any($1::text[])")
        .bind(&deleted_db_names)
        .execute(&mut *conn)
        .await?;

    Ok(deleted_db_names.len())
}
