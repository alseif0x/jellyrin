use jellyrin_db::Database;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let database_url = args.next().expect("database url argument is required");
    let name = args.next().expect("user name argument is required");
    let password = args.next().expect("password argument is required");

    let db = Database::connect(&database_url).await?;
    let user = db.upsert_admin_user(&name, &password).await?;
    println!("{}", user.id);
    Ok(())
}
