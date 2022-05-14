#!/usr/bin/expect
# Copyright (c) Aptos
# SPDX-License-Identifier: Apache-2.0

set timeout 10
set basedir [file normalize [file dirname $argv0]]
cd $basedir

### Test the validator/faucet
spawn /bin/bash
cd $basedir
expect_before {
    timeout { puts "\rERROR: Timeout!\r"; exit 1 }
    eof { puts "\rERROR: eof!\r"; exit 1 }
}

send "cd validator-testnet\r"
send "docker-compose up --remove-orphans\r"

sleep 10

### Ensure validator is started
spawn /bin/bash
cd $basedir
expect_before {
    timeout { puts "\rERROR: Timeout!\r"; exit 1 }
    eof { puts "\rERROR: eof!\r"; exit 1 }
}
send "cd validator-testnet\r"
send "docker-compose logs -f validator\r"
expect "validator_1*Aptos is running"


### Ensure faucet is started
spawn /bin/bash
cd $basedir
expect_before {
    timeout { puts "\rERROR: Timeout!\r"; exit 1 }
    eof { puts "\rERROR: eof!\r"; exit 1 }
}

send "cd validator-testnet\r"
send "docker-compose logs -f faucet\r"
expect "faucet_1*running*"


sleep 5

puts "\rPASSED!\r"
