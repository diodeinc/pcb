.subckt kicad_builtin_vdiff in+ in- out
  Emeas out 0 in+ in- 1
.ends

.subckt kicad_builtin_varistor A B threshold=1k rdyn=1 rleak=100MEG
  B1 A B I=( abs( V( A, B ) ) > {threshold} ?  ( abs( V( A, B ) ) - {threshold} ) / {rdyn} * sgn( V( A, B ) ) : 0 )
  R1 A B {rleak} //Leakage helps with convergence in case of circuit with current sources
.ends

* Simple generic model for a single-pole OpAmp
* Parameters are pole frequency, gain, offset, output resistance.
* The output is limited to the supply voltage.
* Author Holger Vogt, Public Domain
.subckt kicad_builtin_opamp in+ in- vcc vee out params: POLE=20 GAIN=20k VOFF=10m ROUT=10
* add offset voltage
  Voff in+ inoff dc {VOFF}
* gain stage with RC pole
  G10 0 int inoff in- 100u
  R1 int 0 {GAIN/100u}
  C1 int 0 {1/(6.28*(GAIN/100u)*POLE)}
* output decoupling, output resistance
  Eout 2 0 int 0 1
  Rout 2 out {ROUT}
* output limited to vee, vcc
  Elow lee 0 vee 0 1
  Ehigh lcc 0 vcc 0 1
  Dlow lee int Dlimit
  Dhigh int lcc Dlimit
  .model Dlimit D N=0.01
*
.ends

* Simple generic model for a single-pole dual OpAmp
* Parameters are pole frequency, gain, offset, output resistance.
* The output is limited to the supply voltage.
.subckt kicad_builtin_opamp_dual out1 in1- in1+ vee in2+ in2- out2 vcc params: POLE=20 GAIN=20k VOFF=10m ROUT=10
X1 in1+ in1- vcc vee out1 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
X2 in2+ in2- vcc vee out2 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
.ends

* Simple generic model for a single-pole quad OpAmp
* Parameters are pole frequency, gain, offset, output resistance.
* The output is limited to the supply voltage.
.subckt kicad_builtin_opamp_quad out1 in1- in1+ vcc in2+ in2- out2 out3 in3- in3+ vee in4+ in4- out4 params: POLE=20 GAIN=20k VOFF=10m ROUT=10
X1 in1+ in1- vcc vee out1 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
X2 in2+ in2- vcc vee out2 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
X3 in3+ in3- vcc vee out3 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
X4 in4+ in4- vcc vee out4 kicad_builtin_opamp POLE={POLE} GAIN={GAIN} VOFF={VOFF} ROUT={ROUT}
.ends

* Dual Potentiometer model
* Symbol pins 1 2 3 4 5 6
.subckt pot_dual r1A wA r0A r1B wB r0B params: Rtot=1k wpos=0.4
Apot1 r0A wA r1A potmod
Apot2 r0B wB r1B potmod
.model potmod potentiometer(position={wpos} r={Rtot})
.ends
