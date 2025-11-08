

## To start backend 
cargo run




#Initiate postgres and create user and database
 sudo -i -u postgres

CREATE DATABASE hft;
CREATE USER postgres WITH PASSWORD 'test';

#Initiate hft Database

psql -h localhost -U postgres -d hft
## Check status
sudo systemctl status postgresql


## Create table if not create
 CREATE TABLE stock_data (
    id SERIAL PRIMARY KEY,
    stock_id INT NOT NULL,
    price REAL NOT NULL,
    ts TIMESTAMP NOT NULL

## Check created table
 \d stock_data

## Check table values
SELECT * FROM stock_data ORDER BY ts DESC LIMIT 20;


